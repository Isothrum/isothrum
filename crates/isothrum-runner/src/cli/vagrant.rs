//! The Vagrant adapter: its lifecycle commands on a project's machines, its failures read as
//! [`Failure`], and what it says of the machines (`status`, `ssh-config`, `port`, `plugin list`).

use std::fmt;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;
use crate::tool_output::vagrant as said;

/// The tool.
const TOOL: &str = "vagrant";

/// A failed Vagrant call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// A name VirtualBox already holds for a VM or its folder (`VERR_ALREADY_EXISTS`, `Could not
    /// rename the directory`): an earlier run's VM.
    AlreadyExists,
    /// Another process holds the machine's lock, its action lock or the index lock.
    Locked {
        /// Whether the refused call had begun acting on a machine before the refusal (it printed
        /// a `==> <machine>: …` line): a call that provisions or restarts may then not run again.
        acted: bool,
        /// The machines (define names) the refusal names, each once.
        machines: Vec<String>,
    },
    /// The call names a machine the Vagrantfile doesn't define (`MachineNotFound`).
    UnknownMachine,
    /// The provider can't list forwarded ports (`vagrant port` on libvirt).
    PortsUnsupported,
    /// The machine is recorded on another provider (`… with a different provider`, `… a single
    /// provider at a time`).
    OtherProvider,
    /// ESXi refused to halt a VM already off.
    AlreadyOff,
    /// Any other failure: its reason.
    Other(String),
}

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        match said::Failure::of(out.stdout.iter().chain(&out.stderr_tail)) {
            said::Failure::Locked => {
                return Failure::Locked {
                    acted: acted(out),
                    machines: locked_machines(out),
                };
            }
            said::Failure::AlreadyOff => return Failure::AlreadyOff,
            said::Failure::Other => {}
        }
        if said_any(out, &["verr_already_exists", "could not rename"]) {
            Failure::AlreadyExists
        } else if said_any(out, &["different provider", "single provider at a time"]) {
            Failure::OtherProvider
        } else if said_any(out, &["was not found configured for"]) {
            Failure::UnknownMachine
        } else if said_any(out, &["does not support listing forwarded ports"]) {
            Failure::PortsUnsupported
        } else {
            Failure::Other(out.reason())
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::AlreadyExists => f.write_str("an earlier VM holds the name"),
            Failure::Locked { .. } => f.write_str("another process holds the machine's lock"),
            Failure::UnknownMachine => f.write_str("the Vagrantfile defines no such machine"),
            Failure::PortsUnsupported => f.write_str("the provider can't list forwarded ports"),
            Failure::OtherProvider => f.write_str("the machine is recorded on another provider"),
            Failure::AlreadyOff => f.write_str("the VM is already off"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// How Vagrant's human output starts each line it says for a machine (`==> web: Removing
/// domain...`): what it does to the machine is announced so, before it is done.
const ACTING: &str = "==> ";

/// Whether `out`, a failed call, announced an action on a machine on either stream.
fn acted(out: &Captured) -> bool {
    out.stdout.iter().chain(&out.stderr_tail).any(|line| line.starts_with(ACTING))
}

/// The machines (define names) a lock refusal `out` names: the action lock's `on the machine
/// '<define>'`, the index lock's `Name: <define>`, on either stream, each once.
fn locked_machines(out: &Captured) -> Vec<String> {
    let mut named: Vec<String> = Vec::new();
    for line in out.stdout.iter().chain(&out.stderr_tail) {
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

/// The oldest Vagrant the driver takes: 2.4.0, the first that honours a box's architecture
/// (`config.vm.box_architecture`): an older one takes any architecture's build of a box.
pub(crate) const MIN_VERSION: &str = "2.4.0";

/// Whether an `up` provisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provision {
    /// As Vagrant does by default (a machine's first boot).
    Default,
    /// `--provision`.
    Yes,
    /// `--no-provision`.
    No,
}

/// `up` of `machines` (all when empty) on `provider`.
pub(crate) async fn up(call: &Call<'_>, provider: Option<&str>, provision: Provision, machines: &[String]) -> Result<(), CliError<Failure>> {
    let mut args = argv(["up"]);
    if let Some(provider) = provider {
        args.extend(["--provider".to_owned(), provider.to_owned()]);
    }
    match provision {
        Provision::Default => {}
        Provision::Yes => args.push("--provision".into()),
        Provision::No => args.push("--no-provision".into()),
    }
    args.extend(machines.iter().cloned());
    call.ok(TOOL, args, Failure::of).await.map(drop)
}

/// A verb on `machines` (all when empty): `halt`, `suspend`, `provision`, `destroy -f`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    /// `halt`.
    Halt,
    /// `suspend`.
    Suspend,
    /// `provision`.
    #[cfg_attr(not(test), expect(dead_code, reason = "the re-apply of a changed project (no-manifest N9) calls it"))]
    Provision,
    /// `destroy -f`.
    Destroy,
}

/// `verb` on `machines`.
pub(crate) async fn act(call: &Call<'_>, verb: Verb, machines: &[String]) -> Result<(), CliError<Failure>> {
    let mut args = match verb {
        Verb::Halt => argv(["halt"]),
        Verb::Suspend => argv(["suspend"]),
        Verb::Provision => argv(["provision"]),
        Verb::Destroy => argv(["destroy", "-f"]),
    };
    args.extend(machines.iter().cloned());
    call.ok(TOOL, args, Failure::of).await.map(drop)
}

/// Each machine's state label, `status --machine-readable` (read as a state by
/// [`said::machine_state`]).
pub(crate) async fn status(call: &Call<'_>) -> Result<said::Reading, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["status", "--machine-readable"]), Failure::of).await?;
    Ok(said::parse(&out.stdout.join("\n")))
}

/// The address `ssh-config <machine>` gives it; [`CliError::Unexpected`] when it names none.
#[expect(dead_code, reason = "a VM's address in the status (the Vagrant driver's gap (b): ESXi publishes no port here) reads it")]
pub(crate) async fn ssh_host(call: &Call<'_>, machine: &str) -> Result<String, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["ssh-config", machine]), Failure::of).await?;
    said::host_name(&out.stdout.join("\n")).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: format!("an ssh-config for `{machine}` with no HostName"),
    })
}

/// One port Vagrant forwards for a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Forward {
    /// The guest's port.
    pub(crate) guest: u16,
    /// The host's.
    pub(crate) host: u16,
}

/// `port <machine> --machine-readable`'s forwards (`forwarded_port,<guest>,<host>` records).
pub(crate) fn forwards(out: &str) -> Result<Vec<Forward>, String> {
    out.lines()
        .filter_map(said::fields)
        .filter(|(_, kind, _)| *kind == "forwarded_port")
        .map(|(_, _, data)| {
            let (guest, host) = data.split_once(',').ok_or_else(|| format!("a forward that isn't `guest,host`: {data}"))?;
            let port = |p: &str| p.trim().parse::<u16>().map_err(|e| format!("a forward's port `{p}`: {e}"));
            Ok(Forward {
                guest: port(guest)?,
                host: port(host)?,
            })
        })
        .collect()
}

/// The ports Vagrant forwards for `machine`.
pub(crate) async fn port(call: &Call<'_>, machine: &str) -> Result<Vec<Forward>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["port", machine, "--machine-readable"]), Failure::of).await?;
    forwards(&out.stdout.join("\n")).map_err(|what| CliError::Unexpected { tool: TOOL, what })
}

/// The plugins installed, each with its version when listed.
pub(crate) async fn plugins(call: &Call<'_>) -> Result<Vec<(String, Option<String>)>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["plugin", "list", "--machine-readable"]), Failure::of).await?;
    Ok(said::plugins(&out.stdout.join("\n")))
}

/// `vagrant --version`, [`CliError::TooOld`] below [`MIN_VERSION`].
pub(crate) async fn supported_version(call: &Call<'_>) -> Result<crate::project::platform::Version, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["--version"]), Failure::of).await?;
    super::version(TOOL, &out.stdout.join("\n"), Some(MIN_VERSION))
}

/// `ssh <machine> -c <script>` arguments, with no terminal.
fn ssh_args(machine: &str, script: &str) -> Vec<String> {
    argv(["ssh", machine, "-c", script])
}

/// The console into `machine` over `vagrant ssh` (its login shell, or `script` when given), with a
/// terminal, for the embedder to run.
pub(crate) fn console_spec(call: &Call<'_>, machine: &str, script: Option<&str>) -> crate::process::ProcessSpec {
    let args = match script {
        None => argv(["ssh", machine]),
        Some(script) => argv(["ssh", machine, "-c", script]),
    };
    call.spec(TOOL, args)
}

/// `script` run on `machine` over `vagrant ssh -c`, whatever its exit: what it printed and how it
/// ended (a command that fails in the machine is its result, not the adapter's failure).
pub(crate) async fn ssh_run(call: &Call<'_>, machine: &str, script: &str) -> Result<Captured, CliError<Failure>> {
    call.run(TOOL, ssh_args(machine, script)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Exit;

    fn failed(stdout: &str, stderr: &str) -> Failure {
        let lines = |t: &str| t.lines().map(str::to_owned).collect();
        Failure::of(&Captured::new(TOOL, Exit::new(Some(1)), lines(stdout), lines(stderr)))
    }

    #[test]
    fn failures_read_as_vagrant_prints_them() {
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/vagrant/2.4.9-error-machine-locked.txt"), ""),
            Failure::Locked {
                acted: false,
                machines: vec!["probe".to_owned()]
            }
        );
        assert_eq!(
            failed("", include_str!("../tool_output/fixtures/vagrant/vmware-esxi-2.5.5-halt-already-off.txt")),
            Failure::AlreadyOff
        );
        assert_eq!(
            failed("", include_str!("../tool_output/fixtures/vagrant/2.4.1-virtualbox-7.0-already-exists.txt")),
            Failure::AlreadyExists
        );
        assert_eq!(
            failed("", include_str!("../tool_output/fixtures/vagrant/2.4.1-other-provider.txt")),
            Failure::OtherProvider
        );
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/vagrant/2.4.9-machine-not-found.txt"), ""),
            Failure::UnknownMachine
        );
        assert_eq!(
            failed(include_str!("../tool_output/fixtures/vagrant/2.4.9-libvirt-port-unsupported.txt"), ""),
            Failure::PortsUnsupported
        );
        assert!(matches!(failed("", "The box 'nope' could not be found"), Failure::Other(_)));
    }

    #[test]
    fn a_lock_refusal_says_whether_it_had_acted_and_on_which_machines() {
        let action = "1,,error-exit,Vagrant::Errors::MachineActionLockedError,An action 'up' was attempted on the machine 'web'%!(VAGRANT_COMMA)\\nbut another process is already executing an action on the machine.";
        assert_eq!(
            failed(&format!("Bringing machine 'web' up with 'libvirt' provider...\n{action}"), ""),
            Failure::Locked {
                acted: false,
                machines: vec!["web".to_owned()]
            }
        );
        let after = failed(
            "==> web: Removing domain...",
            "Vagrant can't use the requested machine because it is locked! This\nName: web\nName: web",
        );
        assert_eq!(
            after,
            Failure::Locked {
                acted: true,
                machines: vec!["web".to_owned()]
            },
            "an action announced on stdout, the index lock's name on stderr, named once"
        );
    }

    #[test]
    fn forwards_read_as_vagrant_prints_them() {
        let read = forwards(include_str!("../tool_output/fixtures/vagrant/2.4.1-port.txt")).unwrap();
        assert_eq!(read, [Forward { guest: 22, host: 2222 }, Forward { guest: 80, host: 8080 }]);
        assert!(forwards("1,web,forwarded_port,22").is_err(), "a cut record isn't a shorter list");
        assert_eq!(forwards("").unwrap(), Vec::<Forward>::new());
    }
}
