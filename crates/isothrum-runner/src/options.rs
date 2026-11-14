//! What the embedder passes to an operation: the values and secrets its tools get, and its choices.

use std::collections::BTreeMap;

use crate::project::platform::Provider;
use crate::secrets::Secrets;

/// The options of one operation.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct RunOptions {
    /// The Vagrant provider (`vagrant up --provider`), for a Vagrant project.
    pub provider: Option<Provider>,
    /// Sensitive values every tool of the operation gets, as its format reads them
    /// (each by its name; Terraform also gets it as `TF_VAR_<name>`): never stored, never written to a file, masked in every event.
    /// Pass them to every operation.
    pub secrets: Secrets,
    /// What the embedder calls the place the run is on (a server or account id): recorded, so a
    /// later operation asked for elsewhere is refused (`running_elsewhere`).
    pub host_label: Option<String>,
    /// How long the run may live from its `up`: recorded as its expiry ([`crate::RunRecord`]'s
    /// `expires_at`), which the reaper reads ([`crate::Runner::expired`]).
    pub lifetime: Option<std::time::Duration>,
    /// Values every tool of the operation gets, by name, as its format reads them
    /// (Compose's `${NAME}`, a Vagrantfile's `ENV`, Terraform's
    /// `TF_VAR_NAME` too). Never stored: pass them to every operation.
    pub env: BTreeMap<String, String>,
    /// The journaled operation this one is: a row a service queued (`Tx::queue_operation`), which
    /// the operation begins instead of journaling a new one.
    pub operation: Option<crate::store::OperationId>,
}

impl RunOptions {
    /// No value, no secret, nothing recorded about the place.
    #[must_use]
    pub fn new() -> RunOptions {
        RunOptions::default()
    }

    /// Sets variable `name` to `value` for every tool of the operation.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> RunOptions {
        self.env.insert(name.into(), value.into());
        self
    }

    /// Gives the tools the sensitive values `secrets`.
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

    /// Lets the run live `lifetime` from its `up`.
    #[must_use]
    pub fn with_lifetime(mut self, lifetime: std::time::Duration) -> RunOptions {
        self.lifetime = Some(lifetime);
        self
    }
}
