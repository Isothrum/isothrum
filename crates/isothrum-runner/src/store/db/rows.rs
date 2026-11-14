//! What the store's tables hold, as the store hands them out.

use std::fmt;
use std::path::PathBuf;

use isoloom_manifest::Protocol;
use serde::{Deserialize, Serialize};

use super::owner::Owner;
use crate::error::ErrorCode;
use crate::name::RunName;
use crate::state::{Desired, Op, RunState};
use crate::store::{LastObserved, RunRecord};

/// An operation's id: the journal's key, increasing in a home and never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct OperationId(pub i64);

/// A resource row's id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct ResourceId(pub i64);

/// An event's place in the home's event table, increasing across every run: a subscriber resumes
/// after the last one it saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct EventCursor(pub u64);

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A page of the event table ([`super::Tx::events_page`]).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct EventPage {
    /// The events it read, oldest first, each with its cursor.
    pub events: Vec<(EventCursor, crate::events::Event)>,
    /// How many of its rows weren't an event this runner reads (another version's kind).
    pub unreadable: usize,
    /// The cursor of the last row it scanned, read or not: where the next page starts.
    pub last: EventCursor,
}

/// A run, as the store keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunRow {
    /// The run's name.
    pub name: RunName,
    /// The manifest it was last brought up or updated from.
    pub manifest: PathBuf,
    /// The SHA-256 of that manifest's file, hex; empty when it wasn't read (an import).
    pub manifest_digest: String,
    /// What the user wants of it.
    pub desired: Desired,
    /// Where it stands.
    pub state: RunState,
    /// Why it failed, when `state` is `failed`.
    pub failed_cause: Option<ErrorCode>,
    /// The operation whose transition put it there; none after an observation or an import.
    pub state_op: Option<OperationId>,
    /// When it got there.
    pub state_at: String,
    /// The rest the runner keeps about it (its `state` is this row's).
    pub record: RunRecord,
    /// What the runner last saw of it, when it read it.
    pub observed: Option<LastObserved>,
    /// When it last saw it.
    pub observed_at: Option<String>,
}

impl RunRow {
    /// Whether the run stands where the user wants it.
    #[must_use]
    pub fn converged(&self) -> bool {
        self.desired.met_by() == self.state
    }
}

/// How an operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Outcome {
    /// It reached its end without an error.
    Succeeded,
    /// It failed.
    Failed,
    /// It was cut short (an embedder's cancel, Ctrl-C).
    Cancelled,
    /// Its process died before it ended: a later operation found it and closed it.
    Abandoned,
}

/// An operation's end.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Ended {
    /// When.
    pub at: String,
    /// How.
    pub outcome: Outcome,
    /// Its error's code, when it failed.
    pub code: Option<ErrorCode>,
}

/// An operation of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OperationRow {
    /// Its id.
    pub id: OperationId,
    /// Its run.
    pub run: RunName,
    /// What it is.
    pub op: Op,
    /// When it was queued (journaled).
    pub queued_at: String,
    /// When it started; none while it waits in a queue.
    pub started_at: Option<String>,
    /// Its end; none while it runs, or when its process died before it ended.
    pub ended: Option<Ended>,
    /// Whether a cancel was asked for.
    pub cancel_requested: bool,
    /// The process that runs it.
    pub owner: Owner,
    /// The operation this one recovered, when it found one unfinished.
    pub resumed_from: Option<OperationId>,
}

/// Whose a resource is: the provider's (made by a manifest step) or the runner's own, on this
/// computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// Made by a manifest step: a VM, a container, a network, a volume, a module's resource.
    Provider,
    /// A file or process the runner keeps for the run on this computer.
    Local,
}

/// What a resource is. The provider kinds are manifest v2's `resources[].kind`; the local ones
/// the runner's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceKind {
    /// A virtual machine.
    Vm,
    /// A container.
    Container,
    /// A network.
    Network,
    /// A volume.
    Volume,
    /// A hub (a Docker host VM several runs share).
    Hub,
    /// A process a step leaves running.
    Process,
    /// A Terraform module's resources, as one.
    Module,
    /// The machines' SSH host keys this computer trusts.
    HostKeys,
    /// The files the run's tools keep in its state folder.
    StateFiles,
    /// The run's outputs archive.
    Archive,
    /// The graphical consoles the runner chose.
    Consoles,
    /// A tool process an operation runs.
    ToolProcess,
}

impl ResourceKind {
    /// Whose a resource of this kind is.
    #[must_use]
    pub fn scope(self) -> Scope {
        match self {
            ResourceKind::Vm
            | ResourceKind::Container
            | ResourceKind::Network
            | ResourceKind::Volume
            | ResourceKind::Hub
            | ResourceKind::Process
            | ResourceKind::Module => Scope::Provider,
            ResourceKind::HostKeys | ResourceKind::StateFiles | ResourceKind::Archive | ResourceKind::Consoles | ResourceKind::ToolProcess => Scope::Local,
        }
    }

    /// The lifetime a local kind always has (host keys go with the machines that made them,
    /// the rest with the run); none for a provider kind, whose manifest says.
    #[must_use]
    pub fn lifetime(self) -> Option<Lifetime> {
        match self.scope() {
            Scope::Provider => None,
            Scope::Local if self == ResourceKind::HostKeys => Some(Lifetime::Machines),
            Scope::Local => Some(Lifetime::Run),
        }
    }
}

/// Where a resource stands, written ahead of the provider's call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ResourceState {
    /// Written before the call that makes it: it may or may not exist.
    Creating,
    /// The provider made it, with this id.
    Created,
    /// Written before the call that removes it.
    Destroying,
    /// Removed, or found missing.
    Gone,
}

/// What a resource's life is tied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Lifetime {
    /// The run's machines: a machine made again makes it stale (a cleared start forgets it).
    Machines,
    /// The run: only its `down` removes it.
    Run,
}

/// A provider resource's name on the host, rendered with the run's per-copy key (`${run.key}`):
/// the only name a recovery looks a resource up by, so it never matches another copy's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CopyName(String);

impl CopyName {
    /// `rendered`, a name the manifest's template gave with the run's key `key` filled in.
    ///
    /// # Errors
    ///
    /// `rendered` doesn't hold `key` (a bare manifest name, shared by every copy), or `key` is
    /// empty.
    pub fn rendered(rendered: impl Into<String>, key: &str) -> Result<CopyName, NotPerCopy> {
        let rendered = rendered.into();
        if key.is_empty() || !rendered.contains(key) {
            return Err(NotPerCopy { name: rendered });
        }
        Ok(CopyName(rendered))
    }

    /// A name the store wrote, checked when it was.
    pub(crate) fn stored(name: String) -> CopyName {
        CopyName(name)
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A name with no per-copy key in it: another copy of the run could have it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{name}` doesn't hold the run's key: another copy could have it")]
pub struct NotPerCopy {
    /// The name.
    pub name: String,
}

/// A resource the run made or keeps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ResourceRow {
    /// Its id.
    pub id: ResourceId,
    /// Its run.
    pub run: RunName,
    /// The manifest resource's key, or the local thing's.
    pub key: String,
    /// What it is.
    pub kind: ResourceKind,
    /// Whose it is.
    pub scope: Scope,
    /// Its name on the host, for a provider resource: what a recovery finds a `creating` one by.
    pub copy_name: Option<CopyName>,
    /// The provider's id for it (for a local one, its path); none while `creating`.
    pub native_id: Option<String>,
    /// Where it stands.
    pub state: ResourceState,
    /// What its life is tied to.
    pub lifetime: Lifetime,
    /// The operation that made it.
    pub created_by: OperationId,
    /// The operation that removes or removed it.
    pub destroyed_by: Option<OperationId>,
    /// What removing it needs besides its id.
    pub detail: Option<serde_json::Value>,
}

/// A resource about to be made: written, as `creating`, before the call that makes it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NewResource<'a> {
    /// Its run.
    pub run: &'a RunName,
    /// Its key.
    pub key: &'a str,
    /// What it is.
    pub kind: ResourceKind,
    /// Its name on the host (provider resources).
    pub copy_name: Option<CopyName>,
    /// Its lifetime, for a provider kind (a local kind's is its own: [`ResourceKind::lifetime`]).
    pub lifetime: Option<Lifetime>,
    /// The operation making it.
    pub by: OperationId,
    /// What removing it will need besides its id.
    pub detail: Option<serde_json::Value>,
}

impl<'a> NewResource<'a> {
    /// A resource of `kind`, keyed `key`, of `run`, made by `by`.
    #[must_use]
    pub fn new(run: &'a RunName, key: &'a str, kind: ResourceKind, by: OperationId) -> NewResource<'a> {
        NewResource {
            run,
            key,
            kind,
            copy_name: None,
            lifetime: None,
            by,
            detail: None,
        }
    }

    /// Named `name` on the host.
    #[must_use]
    pub fn named(mut self, name: CopyName) -> NewResource<'a> {
        self.copy_name = Some(name);
        self
    }

    /// With lifetime `lifetime` (a provider kind's, from its manifest).
    #[must_use]
    pub fn living(mut self, lifetime: Lifetime) -> NewResource<'a> {
        self.lifetime = Some(lifetime);
        self
    }

    /// With `detail`.
    #[must_use]
    pub fn with_detail(mut self, detail: serde_json::Value) -> NewResource<'a> {
        self.detail = Some(detail);
        self
    }
}

/// A scope held by a process for a while.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Lease {
    /// What it holds (`run:<name>`, `home:slots`, `home:service`).
    pub scope: String,
    /// Who holds it.
    pub owner: Owner,
    /// Since when.
    pub acquired_at: String,
    /// Until when, in seconds since the Unix epoch, unless renewed.
    pub expires_at: u64,
}

/// What asking for a lease gave.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LeaseOutcome {
    /// Taken; `took_over` is the lease a dead or expired holder left.
    Acquired {
        /// The lease taken over, if one was.
        took_over: Option<Lease>,
    },
    /// A live owner holds it.
    Held(Lease),
}

/// A host port a run's operation holds while it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PortReservation {
    /// The port, on the host.
    pub port: crate::port::HostPort,
    /// Its protocol.
    pub protocol: Protocol,
    /// The run.
    pub run: RunName,
    /// The operation holding it, when it was journaled.
    pub operation: Option<OperationId>,
}
