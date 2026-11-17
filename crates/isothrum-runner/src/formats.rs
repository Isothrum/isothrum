//! The format drivers: one per tool (Compose, Vagrant, Terraform), each running a project of its
//! format through a lifecycle fixed for that format, behind one trait ([`FormatDriver`]). A driver
//! knows only its own tool, and talks to it only through that tool's adapters ([`crate::cli`]),
//! which run the tool and read its output into types. The core (the store, the journal, the
//! operations, events, port reservations, processes) knows no tool and calls the drivers through
//! the trait only.
//!
//! - [`ToolCx`]: what a driver gets for one operation: the project, the run's name and key, its
//!   state folder, the hypervisor asked for, the tool's environment, the processes and where the
//!   tool's lines go.
//! - [`Seen`]: what the tools say exists: the machines and their published ports, the module's
//!   outputs, and the run's state derived from them.
//! - [`Found`]: one resource the tool says belongs to the run, which the [`ledger`] records.

pub(crate) mod any;
pub(crate) mod compose;
#[cfg(test)]
pub(crate) mod contract;
pub(crate) mod ledger;
pub(crate) mod terraform;
mod vagrant;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crate::Result;
use crate::cli::{Call, CliError};
use crate::console::ShellCommand;
use crate::error::{Error, TimedOut};
use crate::events::Stream;
use crate::events::WarningCode;
use crate::graphical::{Console, ConsoleOffer, NativeApp, NativeConsole};
use crate::name::RunName;
use crate::process::ProcessRunner;
use crate::project::Project;
use crate::project::platform::Provider;
use crate::state::{Park, RunState};
use crate::status::MachineState;
use crate::store::{Lifetime, ResourceKind};

/// How long a command run in a machine may take (an `exec`, a project's checks, one provisioner run
/// alone): a check is short, so one that hangs (a guest that never answers its SSH) fails as a
/// timeout, with its last line, well before the operation's caller gives up on it.
pub(crate) const CHECK_LIMIT: Duration = Duration::from_secs(300);

/// What picks the host ports a run pins (Compose's ephemeral publishes): a free loopback port no
/// other run holds, reserved for the run until its operation ends, held until dropped.
pub(crate) trait PickPorts: Send + Sync {
    /// A port over `protocol`; none when none is left.
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    fn pick(&self, protocol: crate::project::platform::Protocol) -> std::io::Result<Option<crate::net::HeldPort>>;

    /// Claims the fixed host ports a first start publishes, before anything is made: each
    /// reserved for the run until its operation ends.
    ///
    /// # Errors
    ///
    /// `port_in_use` at the first port another run holds or something on this computer binds.
    fn claim(&self, ports: &[(crate::port::HostPort, crate::project::platform::Protocol)]) -> Result<()>;
}

/// Picks nothing: a context that starts nothing (a console, a status read).
pub(crate) struct NoPorts;

impl PickPorts for NoPorts {
    fn pick(&self, _protocol: crate::project::platform::Protocol) -> std::io::Result<Option<crate::net::HeldPort>> {
        Ok(None)
    }

    fn claim(&self, _ports: &[(crate::port::HostPort, crate::project::platform::Protocol)]) -> Result<()> {
        Ok(())
    }
}

/// What a format driver gets for one operation.
pub(crate) struct ToolCx<'a> {
    /// The project the run runs.
    pub(crate) project: &'a Project,
    /// The run's name: Compose's project name, and what the run's own state is filed under.
    pub(crate) run: &'a RunName,
    /// The run's per-copy key ([`crate::copy_key`]), which the tools also get as
    /// [`crate::RUN_KEY_VAR`].
    pub(crate) key: &'a str,
    /// The folder the runner keeps the run's own files in (Vagrant's dotfile folder, Compose's
    /// pinned ports, Terraform's plugin cache link), owner-only.
    pub(crate) state_dir: &'a Path,
    /// The hypervisor asked for (Vagrant's `--provider`), if any.
    pub(crate) provider: Option<Provider>,
    /// Every variable the tool gets: the run's inputs ([`crate::project::inputs`]), the run's
    /// key and the operation's id.
    pub(crate) env: Vec<(String, String)>,
    /// The runner's own environment, which the tools inherit (`LIBVIRT_DEFAULT_URI`,
    /// `VAGRANT_HOME`).
    pub(crate) host_env: &'a crate::host_env::HostEnv,
    /// What runs the tool.
    pub(crate) processes: &'a dyn ProcessRunner,
    /// The runner's own SSH key: what a module's machine is signed in to with when the run names
    /// no key of its own.
    pub(crate) ssh_key: &'a Path,
    /// What picks the host ports the run pins.
    pub(crate) ports: &'a dyn PickPorts,
    /// Where each line the tool prints goes (the operation's step output events).
    pub(crate) sink: &'a (dyn Fn(Stream, &str) + Send + Sync),
    /// Where a driver says the steps it goes through ([`crate::Phase`]): the operation's step
    /// events.
    pub(crate) steps: crate::steps::Steps<'a>,
    /// Where a driver says what it did that the operation's caller should know (a recovery, a
    /// state discarded, a retry): the operation's warning events.
    pub(crate) warn: &'a (dyn Fn(WarningCode, &str) + Send + Sync),
    /// The keys of the run's resources the store records as made (the provider's, with their id,
    /// not gone), read when asked: what tells a machine removed outside the run from one never
    /// made. An operation records what its tool made before it observes, so a read after it is
    /// current.
    pub(crate) made: &'a (dyn Fn() -> Result<Vec<String>> + Send + Sync),
    /// Where a driver records what its download fetched for the run that wasn't there before (an
    /// image its pull pulled, a box its `up` added: [`crate::downloads`]); none for a context
    /// that records nothing (a read, a console, a test's), whose driver then doesn't look.
    pub(crate) fetched: Option<&'a (dyn Fn(crate::DownloadItem) + Send + Sync)>,
}

impl ToolCx<'_> {
    /// A call of the tool in the project's folder, with the run's environment, cut off after
    /// `limit` without an end.
    pub(crate) fn call(&self, limit: Duration) -> Call<'_> {
        Call {
            runner: self.processes,
            cwd: Some(self.project.dir.clone()),
            env: self.env.clone(),
            limit,
            sink: self.sink,
            host_env: self.host_env,
        }
    }

    /// A read of the tool (its version, a listing, a status) within `limit`: [`ToolCx::call`]
    /// whose lines aren't said in the operation's log. Its failure still is, with its last lines.
    pub(crate) fn read(&self, limit: Duration) -> Call<'_> {
        Call {
            sink: &discard,
            ..self.call(limit)
        }
    }

    /// Variable `name` as the tool sees it: the run's own, else the runner's environment's; none
    /// when unset or empty.
    pub(crate) fn var(&self, name: &str) -> Option<String> {
        self.env
            .iter()
            .rev()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .or_else(|| self.host_env.text(name).map(str::to_owned))
            .filter(|v| !v.is_empty())
    }
}

/// Nothing recorded as made: a context that reads no machine's state (a console, a command), or a
/// test's.
pub(crate) static NOTHING_MADE: &(dyn Fn() -> Result<Vec<String>> + Send + Sync) = &|| Ok(Vec::new());

/// Warns that `tool`, which printed no version the runner reads (`found` none), is used with
/// its minimum (`min`) unchecked.
pub(crate) fn unknown_version(cx: &ToolCx<'_>, found: Option<&crate::project::platform::Version>, tool: &str, min: &str) {
    if found.is_none() {
        let said = format!("`{tool}` printed no version the runner reads: it is used, the {min} it needs at least unchecked");
        (cx.warn)(WarningCode::VersionUnknown, &said);
    }
}

/// Says nothing of a line: a read's.
pub(crate) fn discard(_: Stream, _: &str) {}

/// The runner's error for an adapter's call `e`, made as step `step` of the operation: a missing
/// tool, one too old, one cut off at its limit, one whose answer can't be read, and one that failed
/// with its reason. A driver maps the failures it acts on before it gets here (a recovery, a port
/// taken).
pub(crate) fn tool_error<F: std::fmt::Display>(step: &str, e: CliError<F>) -> Error {
    let step = step.to_owned();
    match e {
        CliError::Missing { tool } => Error::ToolMissing { step, tool: tool.to_owned() },
        CliError::TooOld { tool, found, min } => Error::ToolTooOld {
            tool: tool.to_owned(),
            found,
            required: min,
        },
        CliError::TimedOut { limit_s, last_line, .. } => Error::Timeout {
            step,
            timed_out: TimedOut::Ceiling { after_s: limit_s, last_line },
        },
        // Only a download is watched for stalling, and the only one is an image pull.
        CliError::Stalled { after_s, last_line, .. } => Error::DownloadStalled {
            what: crate::error::DownloadKind::Images,
            step,
            after_s,
            last_line,
        },
        unread @ (CliError::Unexpected { .. } | CliError::Spawn { .. }) => Error::ToolFailed {
            step,
            reason: unread.to_string(),
        },
        CliError::Failed { tool, failure } => Error::ToolFailed {
            step,
            reason: format!("`{tool}` failed: {failure}"),
        },
    }
}

/// What the tools say exists of a run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Seen {
    /// Each machine (a Compose service, a Vagrant machine, a VM a module makes) with its state
    /// and the host ports it publishes, as the tool reports them.
    pub(crate) machines: Vec<SeenMachine>,
    /// What the project gives back (a Terraform module's outputs), by name.
    pub(crate) outputs: BTreeMap<String, Output>,
}

/// One machine as its tool reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SeenMachine {
    /// Its name in the project (a Compose service, a Vagrant machine, a module's VM).
    pub(crate) name: String,
    /// Its state.
    pub(crate) state: MachineState,
    /// Where its published ports answer on the host.
    pub(crate) published: Vec<SeenPort>,
    /// What its tool says it is, when it says: Vagrant's provider (`virtualbox`), a module's
    /// resource type (`proxmox_virtual_environment_vm`). What a driver tells its consoles by.
    pub(crate) kind: Option<String>,
    /// The address it is reached at, when its tool says one another computer reaches: Vagrant's
    /// `ssh-config` on a provider that gives the VM's own, a module's VM's as its state says.
    pub(crate) address: Option<String>,
    /// Its tool says it runs Windows (a module's VM, as its state says: [`crate::cli::terraform::VmFacts::windows`]).
    /// What a driver offers Remote Desktop by.
    pub(crate) windows: bool,
}

/// One published port of a machine, as its tool reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeenPort {
    /// The machine's port.
    pub(crate) guest: crate::port::GuestPort,
    /// The host port it answers at.
    pub(crate) host: crate::port::HostPort,
    /// `tcp` or `udp`, when the tool says (Vagrant's `port` doesn't): never guessed.
    pub(crate) protocol: Option<crate::project::platform::Protocol>,
}

/// What changed in a project between what the run runs and its files now, by machine name: what
/// [`FormatDriver::reapply`] brings the running run to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Change {
    /// Machines the files declare that the run doesn't have.
    pub(crate) added: Vec<String>,
    /// Machines the run has that the files no longer declare.
    pub(crate) removed: Vec<String>,
    /// Machines in both whose definition changed (their tool says which, or every one when it
    /// can't tell).
    pub(crate) changed: Vec<String>,
}

/// One value a project gives back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Output {
    /// The value, as text (JSON for a value that isn't a string).
    pub(crate) value: String,
    /// Whether the project marks it sensitive (never shown, never logged).
    pub(crate) sensitive: bool,
}

/// One resource the tool says belongs to the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Found {
    /// What stays the same for it across its re-creations, unique in the run: Compose's
    /// `container:<service>-<n>`, `network:<name>`; Vagrant's `vm:<machine>`; Terraform's
    /// resource address.
    pub(crate) key: String,
    /// What it is.
    pub(crate) kind: ResourceKind,
    /// The tool's id for it (a container's or VM's id, a module resource's id).
    pub(crate) native_id: String,
    /// What its life is tied to: a machine's re-creation ([`Lifetime::Machines`]) or the run's
    /// end ([`Lifetime::Run`]).
    pub(crate) lifetime: Lifetime,
}

/// A run's result of running a command in one of its machines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Ran {
    /// Its exit status (`-1` when it was killed by a signal).
    pub(crate) exit: i32,
    /// What it printed on its standard output.
    pub(crate) stdout: String,
    /// The end of what it printed on its standard error.
    pub(crate) stderr: String,
}

/// A format's driver: the format's lifecycle, its status, and the resources it makes. Internal
/// and statically dispatched (`async fn`, not object safe by design).
pub(crate) trait FormatDriver {
    /// Refuses a run this computer can't drive before anything is made: the tool missing or too
    /// old, the provider's plugin missing, a provider this host doesn't run.
    async fn probe(&self, cx: &ToolCx<'_>) -> Result<()>;
    /// Whether the tool's calls for a run on `provider` need elevation (Hyper-V's): the core then
    /// hands the driver its elevated process runner. Never by default.
    fn elevates(&self, _provider: Option<Provider>) -> bool {
        false
    }
    /// Whether a status read may find a failed run ready: what it reads is live (containers, VMs
    /// that came up after all). A Terraform state isn't: what a failed apply left in it (an
    /// instance made before the one that failed) proves nothing, so only an apply that ends well
    /// makes the run ready again. `failed_start` says what failed the run was a start (an `up`, a
    /// resume, an update) that didn't end well, or never ended (its process killed). Yes by
    /// default.
    fn ready_by_observation(&self, _failed_start: bool) -> bool {
        true
    }
    /// Makes and starts everything, and returns once the project says it is ready (Compose's
    /// healthchecks, Vagrant's last provisioner, Terraform's apply). What an earlier start left
    /// is recovered by the driver itself, from what its tool says.
    async fn up(&self, cx: &ToolCx<'_>) -> Result<()>;
    /// Removes everything the run made.
    async fn down(&self, cx: &ToolCx<'_>) -> Result<()>;
    /// Stops the machines and keeps them (`mode`: a pause keeps their memory where the tool can).
    async fn park(&self, cx: &ToolCx<'_>, mode: Park) -> Result<()>;
    /// Starts the parked machines again, without setting them up again.
    async fn resume(&self, cx: &ToolCx<'_>) -> Result<()>;
    /// Brings the running run to what the project's files now say.
    async fn update(&self, cx: &ToolCx<'_>) -> Result<()>;
    /// The machines the project's files declare, by name (Compose's services, the
    /// Vagrantfile's machines, the module's VMs), whether or not they run: what a re-apply diffs.
    async fn machines(&self, cx: &ToolCx<'_>) -> Result<Vec<String>>;
    /// Brings the running run to the project's files machine by machine, with the tool's own
    /// incremental apply (Compose's `up` on the files, Vagrant's `up`/`destroy` per machine,
    /// Terraform's plan and apply): `change` names what moved.
    async fn reapply(&self, cx: &ToolCx<'_>, change: &Change) -> Result<()>;
    /// Runs the set-up again, on `machine` or on every machine.
    /// `with`, when given, runs only the provisioner of that name (Vagrant's
    /// `--provision-with`: a project's checks, re-read from the project before they run).
    async fn provision(&self, cx: &ToolCx<'_>, machine: Option<&str>, with: Option<&str>) -> Result<()>;
    /// Starts `machine`, one the project leaves down at `up` (a Compose service under a profile,
    /// a Vagrant machine with `autostart: false`).
    async fn attach(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()>;
    /// Removes `machine` from the running run.
    async fn retire(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()>;
    /// Runs `argv` in `machine`, with no terminal, and returns what it said.
    async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran>;
    /// The command that opens an interactive console into `machine`, as `user` when given: the
    /// embedder runs it in its own terminal.
    async fn console(&self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> Result<ShellCommand>;
    /// What exists of the run, as the tool reports it, and the state that makes the run in.
    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)>;
    /// Every resource the tool says belongs to the run ([`ledger::record_found`] keeps them).
    async fn found(&self, cx: &ToolCx<'_>) -> Result<Vec<Found>>;
    /// Removes the resource recorded under `key` ([`Found::key`]), of `kind`, whose tool id is
    /// `native_id` (a recovery's, an orphan's). Nothing there any more is fine.
    async fn destroy_by_id(&self, cx: &ToolCx<'_>, key: &str, kind: ResourceKind, native_id: &str) -> Result<()>;
    /// What a status says of `machine`'s graphical console, as [`FormatDriver::observe`] saw it
    /// (`seen`: the run's machines and outputs, `machine` among them): its protocol, or `none` and
    /// why, and, when it runs, why it can't be shown now. `None` where the format has no screen to
    /// speak of (a container): the default.
    async fn graphical(&self, _cx: &ToolCx<'_>, _seen: &Seen, _machine: &SeenMachine) -> Result<Option<ConsoleOffer>> {
        Ok(None)
    }
    /// Reaches running `machine`'s graphical console, where its hypervisor says it is, or says
    /// where the SSH tunnel that reaches it goes ([`Console::tunnelled`]), which the core opens.
    /// The core then refuses one off this computer's loopback, and one that doesn't answer there.
    async fn reach_graphical(&self, _cx: &ToolCx<'_>, machine: &str) -> Result<Console> {
        Err(crate::graphical::no_console(machine, "its format has no graphical console"))
    }
    /// The hypervisor's own app the run's machines open in (`seen`: what exists of them); none
    /// where there is none: the default.
    fn native_app(&self, _cx: &ToolCx<'_>, _seen: &Seen) -> Option<NativeApp> {
        None
    }
    /// What opens running `machine` in its hypervisor's own app.
    async fn native_console(&self, _cx: &ToolCx<'_>, machine: &str) -> Result<NativeConsole> {
        Err(crate::graphical::native::no_app(machine))
    }
    /// The service endpoints the deployment publishes beyond this computer's own ports (a
    /// module's `published` output), by name, each at its `host` and `port` as the deployment
    /// says; none by default.
    async fn published(&self, _cx: &ToolCx<'_>) -> Result<Vec<Published>> {
        Ok(Vec::new())
    }

    /// What each running machine uses now, as its tool samples it once; none by default (a tool
    /// that reports no per-machine figure).
    async fn usage(&self, _cx: &ToolCx<'_>) -> Result<Vec<crate::MachineUsage>> {
        Ok(Vec::new())
    }
}

/// A service endpoint a deployment publishes: its name (`<machine>/<service>`) and its address
/// as the deployment gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Published {
    /// Its name.
    pub(crate) name: String,
    /// Its host: a name or an address.
    pub(crate) host: String,
    /// Its port there.
    pub(crate) port: u16,
}
