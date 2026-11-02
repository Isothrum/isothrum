//! The engine: one per embedder. It holds what the launcher kept in process-wide statics (the
//! operations in flight, the per-run locks, the ports being pinned), so two runners in one
//! process never share them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{HostOs, Protocol};

use crate::Result;
use crate::clock::{Clock, SystemClock};
use crate::error::Error;
use crate::events::Event;
use crate::hooks::Hooks;
use crate::manifest::load_manifest;
use crate::name::RunName;
use crate::net::{HostPorts, LoopbackPorts};
use crate::process::{ProcessRunner, SystemProcesses};
use crate::run::Running;
use crate::state::Op;
use crate::store::{RunLocks, RunRecord, Store};

/// How a runner is built: its home, and the ports it reaches the world through.
#[derive(Clone)]
#[non_exhaustive]
pub struct RunnerConfig {
    /// The runner's home (`runs/` under it). The launcher passes its app data folder; the
    /// `isothrum` command line uses `~/.isothrum`.
    pub home: PathBuf,
    /// Starts the tools.
    pub processes: Arc<dyn ProcessRunner>,
    /// The host's ports.
    pub ports: Arc<dyn HostPorts>,
    /// The time.
    pub clock: Arc<dyn Clock>,
    /// The host's operating system (it picks per-host argv and filters steps).
    pub host: HostOs,
    /// The tools' `PATH`, where the macOS relays look for the real `ovftool` and put their
    /// wrapper first (default: the runner's own).
    pub path: Option<String>,
}

impl RunnerConfig {
    /// A runner at `home` on this host: real processes, the loopback, the system's clock.
    pub fn new(home: impl Into<PathBuf>) -> RunnerConfig {
        RunnerConfig {
            home: home.into(),
            processes: Arc::new(SystemProcesses::default()),
            ports: Arc::new(LoopbackPorts),
            clock: Arc::new(SystemClock),
            host: this_host(),
            path: std::env::var("PATH").ok(),
        }
    }
}

impl RunnerConfig {
    /// Starts the tools through `processes` (a fake in tests).
    #[must_use]
    pub fn with_processes(mut self, processes: Arc<dyn ProcessRunner>) -> RunnerConfig {
        self.processes = processes;
        self
    }

    /// Reads the host's ports through `ports`.
    #[must_use]
    pub fn with_ports(mut self, ports: Arc<dyn HostPorts>) -> RunnerConfig {
        self.ports = ports;
        self
    }

    /// Reads the time from `clock`.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> RunnerConfig {
        self.clock = clock;
        self
    }
}

impl std::fmt::Debug for RunnerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunnerConfig")
            .field("home", &self.home)
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// This build's host operating system.
#[must_use]
pub fn this_host() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::Macos
    } else {
        HostOs::Linux
    }
}

/// The runner.
pub struct Runner {
    pub(crate) config: RunnerConfig,
    pub(crate) store: Store,
    pub(crate) hooks: Vec<Arc<dyn Hooks>>,
    pub(crate) locks: RunLocks,
    flight: Mutex<BTreeMap<RunName, Op>>,
    reserved: Mutex<BTreeMap<(u16, Protocol), RunName>>,
}

impl Runner {
    /// A runner with `config`; its home's `runs/` folder is created owner-only.
    ///
    /// # Errors
    ///
    /// Fails with `io` when the home's `runs/` folder can't be created.
    pub fn new(config: RunnerConfig) -> Result<Runner> {
        let store = Store::open(&config.home)?;
        Ok(Runner {
            config,
            store,
            hooks: Vec::new(),
            locks: RunLocks::default(),
            flight: Mutex::default(),
            reserved: Mutex::default(),
        })
    }

    /// Adds the embedder's hooks around lifecycle edges.
    pub fn add_hooks(&mut self, hooks: Arc<dyn Hooks>) {
        self.hooks.push(hooks);
    }

    /// Reads and checks the manifest at `manifest`, as run `name` (default: the run's name).
    ///
    /// # Errors
    ///
    /// Fails as [`load_manifest`](crate::load_manifest) does; with `run_name` when the run's name
    /// can't name a run; with `unsupported` when this runner can't run the manifest (its driver, a
    /// delivery, this host).
    pub fn open(&self, manifest: &Path, name: Option<RunName>) -> Result<Running<'_>> {
        let loaded = load_manifest(manifest)?;
        let name = match name {
            Some(name) => name,
            None => RunName::new(loaded.manifest.environment.name.as_str())?,
        };
        Running::new(self, loaded, name)
    }

    /// Every recorded run, with the state its last operation left it in. Reads files only.
    ///
    /// # Errors
    ///
    /// Fails with `io` when a record can't be read.
    pub fn runs(&self) -> Result<Vec<RunRecord>> {
        self.store.records()
    }

    /// Run `name`'s logged events, oldest first.
    ///
    /// # Errors
    ///
    /// Fails with `io` when the run's event log can't be read.
    pub fn events(&self, name: &RunName) -> Result<Vec<Event>> {
        self.store.events(name)
    }

    /// The public half of the runner's SSH key, one line: what an embedder authorizes where a
    /// run's SSH goes (a Proxmox node it jumps through). The key is made when the runner has none.
    ///
    /// # Errors
    ///
    /// Fails with `tool_missing` when `ssh-keygen` isn't installed, `tool_failed` or `timeout`
    /// when it can't make the key, and `io` when the runner's `ssh/` folder can't be created.
    pub async fn ssh_public_key(&self) -> Result<String> {
        crate::ssh::ensure_key(self.config.processes.as_ref(), &self.config.home).await
    }

    /// The operations in flight in this runner (a quit guard, a UI rehydrating).
    pub fn in_flight(&self) -> Vec<(RunName, Op)> {
        self.flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(n, o)| (n.clone(), *o))
            .collect()
    }

    /// Marks `op` on `name` in flight until the guard drops.
    pub(crate) fn enter(&self, name: &RunName, op: Op) -> Flight<'_> {
        self.flight.lock().unwrap_or_else(PoisonError::into_inner).insert(name.clone(), op);
        Flight {
            runner: self,
            name: name.clone(),
        }
    }

    /// The operation in flight on `name`, if any.
    pub(crate) fn op_of(&self, name: &RunName) -> Option<Op> {
        self.flight.lock().unwrap_or_else(PoisonError::into_inner).get(name).copied()
    }

    /// Reserves `port` for `name` while it starts; false when another run holds it.
    pub(crate) fn reserve(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        let mut reserved = self.reserved.lock().unwrap_or_else(PoisonError::into_inner);
        let holder = reserved.entry((port, protocol)).or_insert_with(|| name.clone());
        holder == name
    }

    /// Whether another run than `name` holds `port`.
    pub(crate) fn reserved_elsewhere(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        self.reserved
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(port, protocol))
            .is_some_and(|h| h != name)
    }

    /// Each `ports` entry (a host port and its protocol) must be free before run `name` starts:
    /// not reserved by another run of this runner starting meanwhile, and not already bound on
    /// this computer.
    ///
    /// # Errors
    ///
    /// Fails with `port_in_use` at the first port that is reserved elsewhere or already bound.
    pub(crate) fn ensure_ports_free(&self, name: &RunName, ports: impl IntoIterator<Item = (u16, Protocol)>) -> Result<()> {
        for (port, protocol) in ports {
            if self.reserved_elsewhere(name, port, protocol) || self.config.ports.in_use(port, protocol) {
                let protocol = if protocol == Protocol::Udp { "udp" } else { "tcp" };
                return Err(Error::PortInUse { port, protocol });
            }
        }
        Ok(())
    }

    /// Frees every port `name` reserved.
    pub(crate) fn release(&self, name: &RunName) {
        self.reserved.lock().unwrap_or_else(PoisonError::into_inner).retain(|_, holder| holder != name);
    }
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("config", &self.config)
            .field("hooks", &self.hooks.len())
            .field("in_flight", &self.in_flight())
            .finish_non_exhaustive()
    }
}

/// An operation in flight; cleared when dropped, whatever the outcome (an error, a cancellation).
pub(crate) struct Flight<'r> {
    runner: &'r Runner,
    name: RunName,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.runner.flight.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.name);
        self.runner.release(&self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(home: &Path) -> Runner {
        Runner::new(RunnerConfig::new(home)).unwrap()
    }

    #[test]
    fn two_runners_share_nothing() {
        // Regression (R5): the launcher's in-flight map, run locks and probe cache were statics,
        // so two embedders (or two tests) in one process saw each other's operations.
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (runner(a.path()), runner(b.path()));
        let name = RunName::new("lab").unwrap();
        let flight = a.enter(&name, Op::Up);
        assert!(a.reserve(&name, 8080, Protocol::Tcp));
        assert_eq!(a.in_flight(), [(name.clone(), Op::Up)]);
        assert_eq!(b.in_flight(), []);
        assert!(b.reserve(&RunName::new("other").unwrap(), 8080, Protocol::Tcp), "b's ports aren't a's");
        drop(flight);
        assert_eq!(a.in_flight(), []);
    }

    #[test]
    fn a_port_reserved_by_one_run_is_refused_to_another_until_released() {
        let home = tempfile::tempdir().unwrap();
        let r = runner(home.path());
        let (one, two) = (RunName::new("one").unwrap(), RunName::new("two").unwrap());
        assert!(r.reserve(&one, 41000, Protocol::Tcp));
        assert!(r.reserve(&one, 41000, Protocol::Tcp), "its own reservation again");
        assert!(!r.reserve(&two, 41000, Protocol::Tcp));
        assert!(r.reserved_elsewhere(&two, 41000, Protocol::Tcp) && !r.reserved_elsewhere(&one, 41000, Protocol::Tcp));
        assert!(r.reserve(&two, 41000, Protocol::Udp), "another protocol");
        r.release(&one);
        assert!(r.reserve(&two, 41000, Protocol::Tcp));
    }
}
