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

pub(crate) mod ledger;
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
use crate::name::RunName;
use crate::process::ProcessRunner;
use crate::project::Project;
use crate::project::platform::Provider;
use crate::state::{Park, RunState};
use crate::status::MachineState;
use crate::store::{Lifetime, ResourceKind};

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
    /// What runs the tool.
    pub(crate) processes: &'a dyn ProcessRunner,
    /// Where each line the tool prints goes (the operation's step output events).
    pub(crate) sink: &'a (dyn Fn(Stream, &str) + Send + Sync),
    /// Where a driver says what it did that the operation's caller should know (a recovery, a
    /// state discarded, a retry): the operation's warning events.
    pub(crate) warn: &'a (dyn Fn(WarningCode, &str) + Send + Sync),
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
        }
    }
}

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
        CliError::TimedOut { limit_s, .. } => Error::Timeout {
            step,
            timed_out: TimedOut::Ceiling { after_s: limit_s },
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
}

/// One published port of a machine, as its tool reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeenPort {
    /// The machine's port.
    pub(crate) guest: u16,
    /// The host port it answers at.
    pub(crate) host: u16,
    /// `tcp` or `udp`, when the tool says (Vagrant's `port` doesn't): never guessed.
    pub(crate) protocol: Option<isoloom_manifest::Protocol>,
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
    async fn provision(&self, cx: &ToolCx<'_>, machine: Option<&str>) -> Result<()>;
    /// Starts `machine`, one the project leaves down at `up` (a Compose service under a profile,
    /// a Vagrant machine with `autostart: false`).
    async fn attach(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()>;
    /// Removes `machine` from the running run.
    async fn retire(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()>;
    /// Runs `argv` in `machine`, with no terminal, and returns what it said.
    async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran>;
    /// The command that opens an interactive console into `machine`, as `user` when given: the
    /// embedder runs it in its own terminal.
    fn console(&self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> Result<ShellCommand>;
    /// What exists of the run, as the tool reports it, and the state that makes the run in.
    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)>;
    /// Every resource the tool says belongs to the run ([`ledger::record_found`] keeps them).
    async fn found(&self, cx: &ToolCx<'_>) -> Result<Vec<Found>>;
    /// Removes the resource recorded under `key` ([`Found::key`]), of `kind`, whose tool id is
    /// `native_id` (a recovery's, an orphan's). Nothing there any more is fine.
    async fn destroy_by_id(&self, cx: &ToolCx<'_>, key: &str, kind: ResourceKind, native_id: &str) -> Result<()>;
}
