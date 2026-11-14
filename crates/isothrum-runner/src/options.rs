//! What the embedder passes to an operation: its params, secrets and choices.

use std::collections::BTreeMap;

use isoloom_manifest::Provider;

use crate::secrets::Secrets;

/// What `up` does when the run already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Occupied {
    /// Resumes a parked run instead of starting it again (leftovers are still removed first).
    #[default]
    ResumeParked,
    /// Removes whatever exists, then starts afresh.
    Replace,
}

/// The options of one operation.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunOptions {
    /// A Vagrant provider among the manifest's (`${run.provider}`).
    pub provider: Option<Provider>,
    /// Non-secret values, by param name.
    pub params: BTreeMap<String, String>,
    /// Secret and input values, by slot name. Never stored: pass them again to an operation that
    /// creates something.
    pub secrets: Secrets,
    /// What the embedder calls the place the run is on (a server or account id): recorded, so a
    /// later operation asked for elsewhere is refused (`running_elsewhere`).
    pub host_label: Option<String>,
    /// What `up` does when the run exists.
    pub occupied: Occupied,
    /// A login (`user@host`) every SSH of the run goes through first: a Proxmox node whose run
    /// bridge this host can't route to. Read by an `up` that starts the run, and recorded: every
    /// later operation (a resume through `up` too) goes the way the run started, whatever its
    /// options say.
    pub ssh_jump: Option<String>,
    /// How long the run may live, for a manifest with an `expiry` (its default otherwise).
    pub lifetime: Option<std::time::Duration>,
    /// Variables every tool of the operation runs with, by name (the run's key under the name a
    /// project reads, say). Never stored: pass them to every operation.
    pub env: BTreeMap<String, String>,
    /// The journaled operation this one is: a row a service queued (`Tx::queue_operation`), which
    /// the operation begins instead of journaling a new one.
    pub operation: Option<crate::store::OperationId>,
}

impl RunOptions {
    /// No param, no secret, nothing recorded about the place; a parked run is resumed.
    #[must_use]
    pub fn new() -> RunOptions {
        RunOptions::default()
    }

    /// Sets param `name` to `value`.
    #[must_use]
    pub fn with_param(mut self, name: impl Into<String>, value: impl Into<String>) -> RunOptions {
        self.params.insert(name.into(), value.into());
        self
    }

    /// Sets every param of `params`.
    #[must_use]
    pub fn with_params(mut self, params: impl IntoIterator<Item = (String, String)>) -> RunOptions {
        self.params.extend(params);
        self
    }

    /// Sets variable `name` to `value` for every tool of the operation.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> RunOptions {
        self.env.insert(name.into(), value.into());
        self
    }

    /// Gives the secret and input slots `secrets`.
    #[must_use]
    pub fn with_secrets(mut self, secrets: Secrets) -> RunOptions {
        self.secrets = secrets;
        self
    }

    /// Runs as the queued operation `id` of the home's journal ([`crate::store::Tx::queue_operation`]).
    #[must_use]
    pub fn with_operation(mut self, id: crate::store::OperationId) -> RunOptions {
        self.operation = Some(id);
        self
    }

    /// Records the place the run is on as `label`.
    #[must_use]
    pub fn with_host_label(mut self, label: impl Into<String>) -> RunOptions {
        self.host_label = Some(label.into());
        self
    }

    /// Runs with Vagrant provider `provider`.
    #[must_use]
    pub fn with_provider(mut self, provider: Provider) -> RunOptions {
        self.provider = Some(provider);
        self
    }

    /// Goes through `login` (`user@host`) for every SSH of the run, once an `up` starting it
    /// records it ([`RunOptions::ssh_jump`]).
    #[must_use]
    pub fn with_ssh_jump(mut self, login: impl Into<String>) -> RunOptions {
        self.ssh_jump = Some(login.into());
        self
    }

    /// Lets the run live `lifetime` (a manifest with an `expiry`).
    #[must_use]
    pub fn with_lifetime(mut self, lifetime: std::time::Duration) -> RunOptions {
        self.lifetime = Some(lifetime);
        self
    }

    /// What `up` does when the run exists.
    #[must_use]
    pub fn with_occupied(mut self, occupied: Occupied) -> RunOptions {
        self.occupied = occupied;
        self
    }
}
