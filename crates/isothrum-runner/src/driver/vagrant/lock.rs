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

use std::future::Future;
use std::time::Duration;

use isoloom_manifest::{Run, Step};

use crate::Result;
use crate::driver::STATUS;
use crate::events::WarningCode;
use crate::executor::{Cx, Echo, Hold, call, capture_output, once, plan, stdout_of};
use crate::process::{Captured, ProcessSpec};

/// How long, after the first refusal, a call refused for another process's lock keeps being
/// tried: a read of the machine holds it for seconds; a process acting on it for longer is no
/// passing read.
pub(super) const PATIENCE: Duration = Duration::from_secs(60);

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
    !out.succeeded() && out.stdout.iter().chain(&out.stderr).any(|line| LOCKED.iter().any(|said| line.contains(said)))
}

/// Whether `vagrant <args>`, refused for another process's lock, may run again, `acted` when the
/// refused try announced an action on a machine ([`ACTING`]). A read, and a command that
/// converges, always: a second `destroy`, `halt` or `suspend` acts only on what the first left;
/// a second `up` boots only what isn't running and provisions only a VM whose provisioning hadn't
/// begun (Vagrant's sentinel, written as it begins: a provisioner set to run always runs at every
/// `up` anyway), unless it is told to provision (`--provision`, `--provision-with`). Any other
/// command it knows (`provision`, `reload`, `resume`, a forced `up`) only when the refused try
/// did nothing yet, else a machine would be provisioned or restarted twice; any command it
/// doesn't know (a script over `ssh`), never.
fn again(args: &[String], acted: bool) -> bool {
    let provisions = args
        .iter()
        .any(|a| a == "--provision" || a == "--provision-with" || a.starts_with("--provision-with="));
    match args.first().map(String::as_str) {
        Some("status" | "destroy" | "halt" | "suspend") => true,
        Some("up") if !provisions => true,
        Some("up" | "provision" | "reload" | "resume") => !acted,
        _ => false,
    }
}

/// Whether `lines` hold an action Vagrant announced on a machine ([`ACTING`]).
fn acted<'a>(mut lines: impl Iterator<Item = &'a String>) -> bool {
    lines.any(|line| line.starts_with(ACTING))
}

/// When a call refused for a lock stops being tried again: [`PATIENCE`] after its first refusal.
#[derive(Debug, Default)]
struct Patience(Option<tokio::time::Instant>);

impl Patience {
    /// After a refusal: whether there is time left for another try, which is then waited for.
    async fn wait(&mut self) -> bool {
        let now = tokio::time::Instant::now();
        let deadline = *self.0.get_or_insert(now + PATIENCE);
        if now + POLL > deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
        true
    }
}

/// `attempt` until it isn't refused for a lock ([`refused`]), as long as `safe` says a refused
/// try may run again and [`Patience`] allows: how the last try ended. `retrying` is told before
/// each new try.
async fn waiting<F, Fut>(what: &str, safe: impl Fn(&Captured) -> bool, mut retrying: impl FnMut(), mut attempt: F) -> Result<Captured>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Captured>>,
{
    let mut patience = Patience::default();
    loop {
        let out = attempt().await?;
        if !refused(&out) || !safe(&out) || !patience.wait().await {
            return Ok(out);
        }
        tracing::debug!(call = what, "another Vagrant process holds a machine's lock: tried again");
        retrying();
    }
}

/// Reads the run's `lifecycle.status` (`vagrant status --machine-readable`): its standard
/// output, once no other process's lock stops it.
pub(super) async fn read_status(cx: &Cx<'_>) -> Result<String> {
    let step: &Step = &cx.run.manifest().lifecycle.status;
    let out = waiting(STATUS, |_| true, || {}, || capture_output(cx, STATUS, step)).await?;
    stdout_of(cx, step.id.as_str(), None, &out)
}

/// [`call`], tried again while another process's lock is all that refused it and [`again`]
/// allows it, said in a warning unless it is a read (`vagrant status`).
pub(super) async fn call_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    let reads = spec.args.first().is_some_and(|verb| verb == "status");
    let retrying = || {
        if !reads {
            warn_retrying(cx, id);
        }
    };
    let safe = |out: &Captured| again(&spec.args, acted(out.stdout.iter()));
    waiting(id, safe, retrying, || call(cx, id, spec, limit, echo)).await
}

/// [`call_retrying`], failing unless it exits 0: its standard output (as
/// [`call_ok`](crate::executor::call_ok)).
pub(super) async fn call_ok_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<String> {
    let out = call_retrying(cx, id, spec, limit, echo).await?;
    stdout_of(cx, id, Some(spec), &out)
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
                announced |= line.starts_with(ACTING);
                observe(line);
            };
            once(cx, list, step, &mut watch, Hold::No).await?
        };
        if !refused(&ended.last) || !again(&args, announced) || !patience.wait().await {
            return ended.result(cx, step);
        }
        cx.stale.rewind(stale);
        warn_retrying(cx, step.id.as_str());
    }
}

/// The arguments of argv step `step` of list `list` on this host, filled, when it runs `vagrant`.
fn vagrant_args(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Option<Vec<String>>> {
    let Run::Argv(argv) = &step.run else { return Ok(None) };
    let command = plan::command(argv, cx.run.runner.config.host, &cx.vars, &plan::at(list, step))?;
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
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(acted(
            lines(&["Bringing machine 'web' up with 'libvirt' provider...", "==> web: Removing domain..."]).iter()
        ));
        assert!(!acted(
            lines(&["Bringing machine 'web' up with 'libvirt' provider...", "    web: SSH address: 1.2.3.4:22"]).iter()
        ));
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
