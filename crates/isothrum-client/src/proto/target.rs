//! Which run a command acts on, and the runner's options it is run with, on the wire.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use isothrum_runner::{Format, Provider, RunName};
use serde::{Deserialize, Serialize};

/// Which run: its project's folder, absolute (the service doesn't share the client's working
/// folder), the name to run it under (default: the folder's), and its format when the folder
/// holds several (default: the one the runner recognizes there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The project's folder (a file in it means the folder).
    pub path: PathBuf,
    /// The run's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<RunName>,
    /// The project's format, for a folder that holds several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
}

impl Target {
    /// The project at `path`, under `name`, its format recognized.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, name: Option<RunName>) -> Target {
        Target {
            path: path.into(),
            name,
            format: None,
        }
    }

    /// The same, as a project of `format`.
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Target {
        self.format = Some(format);
        self
    }
}

/// The runner's options of an operation, on the wire. Secret values travel only over the owner-only endpoint and are
/// never stored or logged.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpOptions {
    /// The Vagrant provider, for a Vagrant project (a run keeps the one it started with).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// Values every tool of the operation gets, by name, as its format reads them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Sensitive values, by name: as `env`, but masked in every event and never stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
    /// What to call the place the run is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// The Docker context the run's `docker` calls go to: given at `up`, recorded with the run,
    /// used by every later operation that gives none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_context: Option<String>,
    /// A status read only: each sensitive output's own value instead of `***`. The service gives
    /// it only over its owner-only endpoint; it is never in an event, never stored. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reveal: bool,
}

impl OpOptions {
    /// Whether it gives nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == OpOptions::default()
    }
}

impl fmt::Debug for OpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpOptions")
            .field("provider", &self.provider)
            .field("env", &self.env)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("host_label", &self.host_label)
            .field("docker_context", &self.docker_context)
            .field("reveal", &self.reveal)
            .finish()
    }
}
