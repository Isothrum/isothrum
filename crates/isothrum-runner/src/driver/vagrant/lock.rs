//! Vagrant's machine locks. Vagrant lets one process at a time act on a machine, and refuses any
//! other at once (`MachineActionLockedError`) rather than waiting. On Hyper-V even reading a
//! machine's state is such an action (a PowerShell call, a second or two), so another Vagrant
//! process merely reading the run's VMs (a `vagrant global-status --prune`, which reads every
//! VM Vagrant knows, or a `vagrant status` in a terminal) made the run's own status read fail,
//! and the start with it. On every provider, each read of a machine's state also records it in
//! Vagrant's machine index under the machine's index lock, which Vagrant takes without waiting
//! either (`MachineLocked`): two `vagrant status` of the same VMs at once, or one as a halt
//! records the VM off, and one of them fails (seen on libvirt, another process reading the run's
//! status as its start ended). The run's own operation holds them far longer (a `vagrant up`
//! provisioning the VMs): a status meanwhile doesn't read at all
//! ([`Running::status`](crate::Running::status)).
//!
//! A call refused for another process's lock is tried again, [`POLL`] apart, for up to
//! [`PATIENCE`] after the first refusal, when trying it again is safe ([`again`]): the run's
//! status reads, and the run's own Vagrant commands, the manifest's steps (`up`, `destroy`,
//! `halt`, `suspend`, `provision`, …, [`run_step`]) and the driver's (the controller's boot and
//! halt, a VM's restart, hot-add and retire). A refusal doesn't always come before Vagrant acted:
//! the action lock (`MachineActionLockedError`) is taken before the action on that machine runs,
//! but the index lock (`MachineLocked`) is taken at every state read, so in the middle of an
//! action too (a VM's boot waits on its state), and when a destroyed machine leaves the index,
//! after the provider removed it (seen on libvirt: `==> web: Removing domain...`, then the
//! refusal); and a command on several machines may have acted on some before another refused it.
//! So a command is run again only when a second run does nothing twice: one that converges
//! (`destroy`, `halt`, `suspend`; `up`, which provisions only a VM it hasn't provisioned yet,
//! unless told to provision), or any of the others when the refused try announced no action on
//! a machine ([`ACTING`]).
//!
//! A query's status read (a status, a console's: [`Cx::query`]) waits less, [`QUERY_PATIENCE`],
//! and not at all once an operation holds the run (that operation's `vagrant up` holds the VMs
//! for minutes): its refusal is then no failure, but the machines Vagrant named busy
//! ([`read_status`]).

use std::future::Future;
use std::time::Duration;

use isoloom_manifest::{Run, Step};

use crate::Result;
use crate::driver::STATUS;
use crate::events::WarningCode;
use crate::executor::{Cx, Echo, Hold, call, capture_output, once, plan, stdout_of_call, stdout_of_step};
use crate::process::{Captured, ProcessSpec};

/// How long, after the first refusal, a call refused for another process's lock keeps being
/// tried: a read of the machine holds it for seconds; a process acting on it for longer is no
/// passing read.
pub(super) const PATIENCE: Duration = Duration::from_secs(60);

/// How long a query's status read keeps being tried after its first refusal: a passing read of
/// another process holds a machine for seconds, and the app polling the run waits meanwhile.
pub(super) const QUERY_PATIENCE: Duration = Duration::from_secs(10);

/// How long between two tries.
const POLL: Duration = Duration::from_secs(2);

/// What Vagrant says when another process holds a machine's lock, its action lock or its index
/// lock: each error's class, which `--machine-readable` prints in its `error-exit` record, and its
/// message, which a human-readable call prints on standard error (the record repeats it too).
const LOCKED: [&str; 4] = [
    "Vagrant::Errors::MachineActionLockedError",
    "another process is already executing an action on the machine",
    "Vagrant::Errors::MachineLocked,",
    "Vagrant can't use the requested machine because it is locked!",
];

/// How Vagrant's human output starts each line it says for a machine (`==> web: Removing
/// domain...`): what it does to the machine is announced so, before it is done.
const ACTING: &str = "==> ";

/// Whether `out`, a failed Vagrant call, was refused because another process held a machine's
/// lock.
pub(super) fn refused(out: &Captured) -> bool {
    !out.success() && out.stdout.iter().chain(&out.stderr).any(|line| LOCKED.iter().any(|said| line.contains(said)))
}

/// What a `vagrant` command does when it is run a second time: the one reading of its verb, for
/// whether a refused try may run again ([`again`]) and for what is said of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `status`: reads, acts on no machine (tried again without a word).
    Read,
    /// `destroy`, `halt`, `suspend`, and an `up` not told to provision: a second run acts only on
    /// what the first left. A second `up` boots only what isn't running and provisions only a VM
    /// whose provisioning hadn't begun (Vagrant's sentinel, written as it begins: a provisioner
    /// set to run always runs at every `up` anyway).
    Converges,
    /// `provision`, `reload`, `resume`, and an `up` told to provision (`--provision`,
    /// `--provision-with`): a second run provisions or restarts a machine the first already did.
    Repeats,
}

/// What `vagrant <args>` does when run twice; `None` for a command the retry doesn't know (a
/// script over `ssh`), which is never run again.
fn verb(args: &[String]) -> Option<Verb> {
    let provisions = args
        .iter()
        .any(|a| a == "--provision" || a == "--provision-with" || a.starts_with("--provision-with="));
    match args.first().map(String::as_str) {
        Some("status") => Some(Verb::Read),
        Some("destroy" | "halt" | "suspend") => Some(Verb::Converges),
        Some("up") if !provisions => Some(Verb::Converges),
        Some("up" | "provision" | "reload" | "resume") => Some(Verb::Repeats),
        _ => None,
    }
}

/// Whether `vagrant <args>`, refused for another process's lock, may run again, `acted` when the
/// refused try announced an action on a machine ([`announces`]): a read and a command that
/// converges always; one that repeats only when the refused try did nothing yet, else a machine
/// would be provisioned or restarted twice; a command the retry doesn't know, never.
fn again(args: &[String], acted: bool) -> bool {
    match verb(args) {
        Some(Verb::Read | Verb::Converges) => true,
        Some(Verb::Repeats) => !acted,
        None => false,
    }
}

/// Whether `line`, one a Vagrant call printed on either stream, announces an action on a machine
/// ([`ACTING`]).
fn announces(line: &str) -> bool {
    line.starts_with(ACTING)
}

/// Whether `out`, a refused try, announced an action on a machine on either stream (the one rule
/// [`run_step`] applies to every line it watches).
fn acted(out: &Captured) -> bool {
    out.stdout.iter().chain(&out.stderr).any(|line| announces(line))
}

/// When a call refused for a lock stops being tried again: its patience ([`PATIENCE`], a
/// query's [`QUERY_PATIENCE`]) after its first refusal.
#[derive(Debug)]
struct Patience {
    /// How long after the first refusal.
    lasts: Duration,
    /// When it ends, from the first refusal on.
    deadline: Option<tokio::time::Instant>,
}

impl Default for Patience {
    fn default() -> Patience {
        Patience::of(PATIENCE)
    }
}

impl Patience {
    /// A patience of `lasts` after the first refusal.
    fn of(lasts: Duration) -> Patience {
        Patience { lasts, deadline: None }
    }

    /// After a refusal, waits for the next try and says whether there was time for one: false
    /// when the patience is spent (nothing waited).
    async fn wait_for_retry(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        let deadline = *self.deadline.get_or_insert(now + self.lasts);
        if now + POLL > deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
        true
    }
}

/// `attempt` until it isn't refused for a lock ([`refused`]), as long as `safe` says a refused
/// try may run again and `patience` allows: how the last try ended. `retrying` is told before
/// each new try.
async fn waiting<F, Fut>(what: &str, mut patience: Patience, safe: impl Fn(&Captured) -> bool, mut retrying: impl FnMut(), mut attempt: F) -> Result<Captured>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Captured>>,
{
    loop {
        let out = attempt().await?;
        if !refused(&out) || !safe(&out) || !patience.wait_for_retry().await {
            return Ok(out);
        }
        tracing::debug!(call = what, "another Vagrant process holds a machine's lock: tried again");
        retrying();
    }
}

/// What a status read gave: Vagrant's answer, or, for a query, its refusal for another
/// process's lock on these machines (their define names; none when Vagrant named none).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum StatusRead {
    /// Its standard output.
    Read(String),
    /// Refused for as long as the query waits, or until an operation held the run.
    Busy(Vec<String>),
}

/// Reads the run's `lifecycle.status` (`vagrant status --machine-readable`): its standard
/// output, once no other process's lock stops it. An operation's read refused past [`PATIENCE`]
/// fails with Vagrant's refusal; a query's ([`Cx::query`]) is tried for [`QUERY_PATIENCE`], not
/// again once an operation holds the run, and is then [`StatusRead::Busy`].
pub(super) async fn read_status(cx: &Cx<'_>) -> Result<StatusRead> {
    let step: &Step = &cx.manifest().lifecycle.status;
    let patience = Patience::of(if cx.query { QUERY_PATIENCE } else { PATIENCE });
    // An operation that holds the run meanwhile holds its VMs for as long as it acts.
    // A lock file that can't be read says nothing about the run: traced, read as not held, so
    // the query goes on trying rather than giving up on a read error (Clean Code ch.7, 2026-11-13).
    let still = |_: &Captured| {
        !cx.query
            || !cx.run.runner.store.held(cx.run.name()).unwrap_or_else(|e| {
                tracing::warn!(error = %e, run = %cx.run.name(), "the run's lock couldn't be read; read as not held");
                false
            })
    };
    let out = waiting(STATUS, patience, still, || {}, || capture_output(cx, STATUS, step)).await?;
    if cx.query && refused(&out) {
        return Ok(StatusRead::Busy(locked_machines(&out)));
    }
    stdout_of_step(cx, step.id.as_str(), &out).map(StatusRead::Read)
}

/// [`read_status`] for an operation: Vagrant's answer, or its refusal as the read's failure.
pub(super) async fn read_status_output(cx: &Cx<'_>) -> Result<String> {
    match read_status(cx).await? {
        StatusRead::Read(out) => Ok(out),
        StatusRead::Busy(_) => Err(crate::error::Error::ToolFailed {
            step: STATUS.to_owned(),
            reason: "another Vagrant process holds the run's machines".to_owned(),
        }),
    }
}

/// The machines (define names) a lock refusal `out` names: the action lock's `on the machine
/// '<define>'`, the index lock's `Name: <define>`, on either stream, each once.
pub(super) fn locked_machines(out: &Captured) -> Vec<String> {
    let mut named = Vec::new();
    for line in out.stdout.iter().chain(&out.stderr) {
        // A machine-readable record keeps the message on one line, its newlines written `\n`.
        for part in line.split("\\n") {
            let found = part
                .split_once("on the machine '")
                .and_then(|(_, rest)| rest.split_once('\''))
                .map(|(define, _)| define)
                .or_else(|| part.trim().strip_prefix("Name:").map(str::trim));
            if let Some(define) = found.filter(|d| !d.is_empty())
                && !named.iter().any(|n| n == define)
            {
                named.push(define.to_owned());
            }
        }
    }
    named
}

/// [`call`], tried again while another process's lock is all that refused it and [`again`]
/// allows it, said in a warning unless it is a read (`vagrant status`).
pub(super) async fn call_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    let reads = verb(&spec.args) == Some(Verb::Read);
    let retrying = || {
        if !reads {
            warn_retrying(cx, id);
        }
    };
    let safe = |out: &Captured| again(&spec.args, acted(out));
    waiting(id, Patience::default(), safe, retrying, || call(cx, id, spec, limit, echo)).await
}

/// [`call_retrying`], failing unless it exits 0: its standard output (as
/// [`call_ok`](crate::executor::call_ok)).
pub(super) async fn call_ok_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<String> {
    let out = call_retrying(cx, id, spec, limit, echo).await?;
    stdout_of_call(cx, id, spec, &out)
}

/// Argv step `step` of list `list`, its lines shown and `observe`d as any step's: a `vagrant`
/// command another process's lock refused runs again as [`again`] allows, [`POLL`] apart, for up
/// to [`PATIENCE`] after the first refusal, each new try said in a warning. A refused try's lines
/// stay in the step's log, but the stale markers they hold don't count (Vagrant's "is locked" is
/// another process's lock, no leftover of an earlier run): only the last try's do.
pub(crate) async fn run_step(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
    let Some(args) = vagrant_args(cx, list, step)? else {
        return once(cx, list, step, observe, Hold::No).await?.result(cx, step);
    };
    let mut patience = Patience::default();
    loop {
        let stale = cx.stale.seen();
        let mut announced = false;
        let ended = {
            let mut watch = |line: &str| {
                announced |= announces(line);
                observe(line);
            };
            once(cx, list, step, &mut watch, Hold::No).await?
        };
        if !refused(&ended.last) || !again(&args, announced) || !patience.wait_for_retry().await {
            return ended.result(cx, step);
        }
        cx.stale.rewind(stale);
        warn_retrying(cx, step.id.as_str());
    }
}

/// The arguments of argv step `step` of list `list` on this host, filled, when it runs `vagrant`.
fn vagrant_args(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Option<Vec<String>>> {
    let Run::Argv(argv) = &step.run else { return Ok(None) };
    let command = plan::command(argv, cx.scope.host, &cx.vars, &plan::at(list, step))?;
    Ok((command.program == "vagrant").then_some(command.args))
}

/// Says that step `id`'s Vagrant command runs again, another process having held a machine's lock.
fn warn_retrying(cx: &Cx<'_>, id: &str) {
    cx.events.warning(
        WarningCode::Retrying,
        &format!("step `{id}`: another Vagrant process held a machine's lock; running the command again"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Exit;

    fn ended(code: i32, stdout: &[&str], stderr: &[&str]) -> Captured {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Captured {
            exit: Exit::new(Some(code)),
            stdout: lines(stdout),
            stderr: lines(stderr),
        }
    }

    #[test]
    fn only_a_lock_held_by_another_process_is_refused() {
        // `--machine-readable`: the error's class in its `error-exit` record.
        let read = "1,,error-exit,Vagrant::Errors::MachineActionLockedError,An action 'read_state' was attempted";
        assert!(refused(&ended(1, &[read], &[])));
        // Human output: its message, on standard error.
        let halt = "but another process is already executing an action on the machine.";
        assert!(refused(&ended(1, &[], &["An action 'halt' was attempted on the machine 'web',", halt])));
        // Any other failure, and a success that merely prints the words, are not.
        assert!(!refused(&ended(
            1,
            &[],
            &["SSH connection was refused! This usually happens if the VM failed to"]
        )));
        assert!(!refused(&ended(1, &["1,,error-exit,Vagrant::Errors::VBoxManageError,locked"], &[])));
        assert!(!refused(&ended(1, &["1,,error-exit,Vagrant::Errors::MachineLockedOut,x"], &[])));
        assert!(!refused(&ended(0, &[read], &[])));
    }

    /// A refused command runs again only when a second run does nothing twice.
    #[test]
    fn only_what_a_second_run_does_nothing_twice_with_is_run_again() {
        let args = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        for converges in [
            "status --machine-readable",
            "destroy --force",
            "destroy -f web",
            "halt",
            "suspend web",
            "up --provider libvirt",
        ] {
            assert!(again(&args(converges), true), "{converges}");
        }
        assert!(again(&args("up web --no-provision"), true), "an up that never provisions");
        for twice in [
            "provision web",
            "reload web --no-provision",
            "resume web",
            "up --provider libvirt --provision",
            "up --provider libvirt --provision-with setup",
            "up --provision-with=setup",
        ] {
            assert!(again(&args(twice), false), "{twice}: refused before it acted");
            assert!(!again(&args(twice), true), "{twice}: refused once it had acted");
        }
        for never in ["ssh web -c true", "ssh-config web", "plugin list"] {
            assert!(!again(&args(never), false), "{never}");
        }
        assert_eq!(verb(&args("status --machine-readable")), Some(Verb::Read));
        assert_eq!(verb(&args("up web")), Some(Verb::Converges));
        assert_eq!(verb(&args("up web --provision")), Some(Verb::Repeats));
        assert_eq!(verb(&args("ssh web -c true")), None);
    }

    /// A refused try's action line counts on either stream: a driver's own call (`call_retrying`)
    /// reads the lines as the manifest's steps (`run_step`) watch them.
    #[test]
    fn an_action_announced_on_either_stream_counts_as_acted() {
        let booting = "Bringing machine 'web' up with 'libvirt' provider...";
        assert!(acted(&ended(1, &[booting, "==> web: Removing domain..."], &[])));
        assert!(acted(&ended(1, &[booting], &["==> web: Removing domain..."])), "on standard error too");
        assert!(!acted(&ended(1, &[booting, "    web: SSH address: 1.2.3.4:22"], &[])));
    }

    /// Regression: Vagrant's machine index lock, which every read of a machine's state takes
    /// without waiting, refused a status read on libvirt (`MachineLocked`), and the read wasn't
    /// tried again: neither its class nor its message is `MachineActionLockedError`'s.
    #[test]
    fn a_machines_index_lock_held_by_another_process_is_refused() {
        // `vagrant status --machine-readable`, as Vagrant 2.4.9 printed it on libvirt.
        let said = "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant \
                    process is currently reading or modifying\\nthe machine.";
        let fog = "[fog][WARNING] Unrecognized arguments: libvirt_ip_command";
        let read = format!("1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}");
        assert!(refused(&ended(1, &[&format!("1794342958,,ui,error,{said}"), &read], &[fog])));
        // A `vagrant halt` (human output): the message, on standard error.
        let halt = [
            fog,
            "Vagrant can't use the requested machine because it is locked! This",
            "means that another Vagrant process is currently reading or modifying",
        ];
        assert!(refused(&ended(1, &[], &halt)));
    }
}
