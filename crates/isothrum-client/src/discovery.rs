//! Where a home's service is: the files it keeps in the home, and `service.json`, which says
//! where it listens. A client finds the endpoint only there, on every platform.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The files of a home's service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePaths {
    home: PathBuf,
}

impl ServicePaths {
    /// The service files of `home`.
    #[must_use]
    pub fn new(home: impl Into<PathBuf>) -> ServicePaths {
        ServicePaths { home: home.into() }
    }

    /// The home.
    #[must_use]
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The file the service holds an OS lock on while it runs: one service per home.
    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.home.join("service.lock")
    }

    /// The file a client holds an OS lock on while it replaces the home's service
    /// ([`Client::connect_exact`](crate::Client::connect_exact)): two replace it in turn.
    #[must_use]
    pub fn replace_lock(&self) -> PathBuf {
        self.home.join("service.replace.lock")
    }

    /// Where the running service says who and where it is ([`ServiceInfo`]).
    #[must_use]
    pub fn info(&self) -> PathBuf {
        self.home.join("service.json")
    }

    /// The Unix domain socket a service on Unix listens on.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        self.home.join("service.sock")
    }

    /// The log a started service writes (its standard output and error).
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.home.join("service.log")
    }

    /// The running service's [`ServiceInfo`], when `service.json` is there and reads as one (it
    /// may be a dead service's: connecting tells).
    #[must_use]
    pub fn read_info(&self) -> Option<ServiceInfo> {
        let text = std::fs::read_to_string(self.info()).ok()?;
        serde_json::from_str(&text).ok()
    }
}

/// What a running service writes to `<home>/service.json` once it listens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceInfo {
    /// Its process id.
    pub pid: u32,
    /// When it started (RFC 3339, UTC).
    pub started: String,
    /// Its version.
    pub version: String,
    /// The protocol it speaks.
    pub protocol: u32,
    /// Where it listens: the socket's path on Unix, the named pipe's name on Windows.
    pub endpoint: String,
    /// Its life: a random number each start draws.
    pub epoch: u64,
}
