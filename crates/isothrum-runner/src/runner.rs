//! The engine: one per embedder. It holds the operations in flight, the per-run locks and the
//! ports being pinned, never in process-wide statics, so two runners in one process never share
//! them.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{Arch, HostOs, Protocol};

use crate::Result;
use crate::clock::{Clock, SystemClock};
use crate::events::EventLog;
use crate::host_env::HostEnv;
use crate::manifest::load_manifest;
use crate::name::RunName;
use crate::net::{HostPorts, LoopbackPorts, PortReservations};
use crate::process::{ProcessRunner, SystemProcesses};
use crate::run::Running;
use crate::state::Op;
use crate::store::{RunLocks, RunRecord, Store};

/// How a runner is built: its home, and the ports it reaches the world through. Start from
/// [`RunnerConfig::new`] and set a field to change it (`config.processes = Arc::new(fake)`).
#[derive(Clone)]
#[non_exhaustive]
pub struct RunnerConfig {
    /// The runner's home (`runs/` under it). An app passes its data folder; the
    /// `isothrum` command line uses `~/.isothrum`.
    pub home: PathBuf,
    /// Starts the tools.
    pub processes: Arc<dyn ProcessRunner>,
    /// The host's ports.
    pub ports: Arc<dyn HostPorts>,
    /// The time.
    pub(crate) clock: Arc<dyn Clock>,
    /// The host's operating system (it picks per-host argv and filters steps).
    pub host: HostOs,
    /// The `PATH` the runner hands on where it isn't inherited: to an elevated Hyper-V session,
    /// and to Vagrant on an ESXi host from macOS, whose relays look for the real `ovftool` in it
    /// and put their wrapper first (default: the runner's own). The tools the runner starts as
    /// its children inherit its environment as it is.
    pub path: Option<OsString>,
    /// The user's own Terraform plugin cache (`TF_PLUGIN_CACHE_DIR`), which the runs use as it
    /// is; without one, they share one under the home.
    pub(crate) tf_plugin_cache: Option<PathBuf>,
    /// The variables of the runner's environment it hands on where they aren't inherited (the
    /// proxies, the CA bundles: [`HostEnv`]).
    pub(crate) host_env: HostEnv,
    /// The host's CPU architecture: which build of a Vagrant box counts as there already.
    pub(crate) arch: Arch,
}

/// The PATH the tools are found on: this process's, and on Windows the folders VirtualBox and
/// VMware Workstation install to without adding themselves to PATH (`VBOX_MSI_INSTALL_PATH`,
/// `%ProgramFiles%\Oracle\VirtualBox`, `%ProgramFiles(x86)%\VMware\VMware Workstation`), each
/// once, after what is there. Elsewhere, the PATH as it is.
fn tool_path(path: Option<OsString>, var: impl Fn(&str) -> Option<OsString>) -> Option<OsString> {
    if !cfg!(windows) {
        return path;
    }
    let mut dirs: Vec<PathBuf> = path.as_ref().map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    let mut extra: Vec<PathBuf> = Vec::new();
    if let Some(vbox) = var("VBOX_MSI_INSTALL_PATH") {
        extra.push(PathBuf::from(vbox));
    }
    if let Some(pf) = var("ProgramFiles") {
        extra.push(PathBuf::from(pf).join("Oracle").join("VirtualBox"));
    }
    if let Some(pf86) = var("ProgramFiles(x86)") {
        extra.push(PathBuf::from(pf86).join("VMware").join("VMware Workstation"));
    }
    for dir in extra {
        if !dirs.iter().any(|d| d.as_os_str().eq_ignore_ascii_case(dir.as_os_str())) {
            dirs.push(dir);
        }
    }
    std::env::join_paths(dirs).ok().or(path)
}

impl RunnerConfig {
    /// A runner at `home` on this host: real processes (told the Docker CLI's config, see
    /// [`SystemProcesses`]), the loopback, the system's clock, and what it needs of this process's
    /// environment, read now (`PATH`, `TF_PLUGIN_CACHE_DIR`, the proxies, the CA bundles,
    /// Terraform's CLI configuration, Vagrant's variables, `USERPROFILE`).
    pub fn new(home: impl Into<PathBuf>) -> RunnerConfig {
        RunnerConfig {
            home: home.into(),
            processes: Arc::new(SystemProcesses::default()),
            ports: Arc::new(LoopbackPorts),
            clock: Arc::new(SystemClock),
            host: this_host(),
            path: tool_path(std::env::var_os("PATH"), |name| std::env::var_os(name)),
            tf_plugin_cache: std::env::var_os("TF_PLUGIN_CACHE_DIR").filter(|d| !d.is_empty()).map(PathBuf::from),
            host_env: HostEnv::capture(),
            arch: this_arch(),
        }
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
pub(crate) fn this_host() -> HostOs {
    if cfg!(windows) {
        HostOs::Windows
    } else if cfg!(target_os = "macos") {
        HostOs::Macos
    } else {
        HostOs::Linux
    }
}

/// This build's CPU architecture, as Vagrant names a box's build (an x86-64 build reads as
/// `amd64`, an ARM one as `arm64`).
#[must_use]
pub(crate) fn this_arch() -> Arch {
    if cfg!(target_arch = "aarch64") { Arch::Arm64 } else { Arch::Amd64 }
}

/// The runner.
pub struct Runner {
    pub(crate) config: RunnerConfig,
    pub(crate) store: Store,
    pub(crate) locks: RunLocks,
    flight: Mutex<BTreeMap<RunName, Op>>,
    /// The host ports its runs hold while they start.
    reservations: PortReservations,
}

impl Runner {
    /// A runner with `config`; its home (with its missing parents) and the home's `runs/` folder
    /// are created, owner-only (on Windows, a home outside the user's profile gets an owner-only
    /// DACL).
    ///
    /// # Errors
    ///
    /// Fails with `io` when the home can't be created or made owner-only (on Windows, its owner
    /// can't be read or its DACL set), or its `runs/` folder can't be created.
    pub fn new(config: RunnerConfig) -> Result<Runner> {
        let store = Store::open(&config.home, config.host_env.get("USERPROFILE").map(Path::new))?;
        let reservations = PortReservations::new(Arc::clone(&config.ports));
        Ok(Runner {
            config,
            store,
            locks: RunLocks::default(),
            flight: Mutex::default(),
            reservations,
        })
    }

    /// Reads and checks the manifest at `manifest`, as run `name` (default: the manifest's
    /// `environment.name`).
    ///
    /// # Errors
    ///
    /// Fails as [`load_manifest`](crate::load_manifest) does; with `run_name` when the run's name
    /// can't name a run; with `unsupported` when this runner can't run the manifest here (its target
    /// doesn't run on this host, or it has a delivery its driver doesn't take).
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

    /// Run `name`'s record, with the state its last operation left it in; `None` for a run never
    /// recorded. Reads one file: an embedder asking after one run doesn't fail on another run's
    /// record it can't read, as it would reading [`Runner::runs`].
    ///
    /// # Errors
    ///
    /// Fails with `io` when the record is there and can't be read.
    pub fn run_record(&self, name: &RunName) -> Result<Option<RunRecord>> {
        self.store.record(name)
    }

    /// Run `name`'s logged events, oldest first, with how many of its lines it couldn't read as
    /// one (written by another version of the runner, or cut short).
    ///
    /// # Errors
    ///
    /// Fails with `io` when the run's event log can't be read.
    pub fn events(&self, name: &RunName) -> Result<EventLog> {
        self.store.events(name)
    }

    /// The public half of the runner's SSH key, one line: what an embedder authorizes where a
    /// run's SSH goes (a Proxmox node it jumps through). The key is made when the runner has none.
    ///
    /// # Errors
    ///
    /// Fails with `tool_missing` when `ssh-keygen` isn't installed, `tool_failed` or `timeout`
    /// when it can't make the key, and `io` when the runner's `ssh/` folder can't be created or
    /// its public key can't be read.
    pub async fn ssh_public_key(&self) -> Result<String> {
        crate::ssh::ensure_key(self.config.processes.as_ref(), &self.config.home).await
    }

    /// The operations in flight in this runner.
    pub(crate) fn in_flight(&self) -> Vec<(RunName, Op)> {
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
        self.reservations.reserve(name, port, protocol)
    }

    /// A free loopback port over `protocol` reserved for `name` ([`PortReservations::pick_reserved`]).
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_reserved(&self, name: &RunName, protocol: Protocol) -> std::io::Result<Option<crate::net::HeldPort>> {
        self.reservations.pick_reserved(name, protocol)
    }

    /// A free loopback port over `protocol` no other run holds, not reserved
    /// ([`PortReservations::pick_unreserved`]).
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_unreserved(&self, name: &RunName, protocol: Protocol) -> std::io::Result<Option<crate::net::HeldPort>> {
        self.reservations.pick_unreserved(name, protocol)
    }

    /// Each `ports` entry must be free before run `name` starts ([`PortReservations::ensure_free`]).
    ///
    /// # Errors
    ///
    /// Fails with `port_in_use` at the first port that is reserved elsewhere or already bound.
    pub(crate) fn ensure_ports_free(&self, name: &RunName, ports: impl IntoIterator<Item = (u16, Protocol)>) -> Result<()> {
        self.reservations.ensure_free(name, ports)
    }

    /// Whether another run than `name` holds `port` (the tests read it; the operations go
    /// through the picks).
    #[cfg(test)]
    pub(crate) fn reserved_elsewhere(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        self.reservations.reserved_elsewhere(name, port, protocol)
    }

    /// Frees every port `name` reserved.
    pub(crate) fn release(&self, name: &RunName) {
        self.reservations.release(name);
    }
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("config", &self.config)
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
        // Regression: the in-flight map and the port reservations kept in statics let two
        // embedders (or two tests) in one process see each other's operations and ports.
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (a, b) = (runner(a.path()), runner(b.path()));
        let name = RunName::new("demo").unwrap();
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

#[cfg(all(test, windows))]
mod tool_path_tests {
    use super::tool_path;
    use std::ffi::OsString;

    #[test]
    fn the_hypervisors_folders_join_the_path_once() {
        let var = |name: &str| match name {
            "VBOX_MSI_INSTALL_PATH" => Some(OsString::from(r"D:\VBox\")),
            "ProgramFiles" => Some(OsString::from(r"C:\Program Files")),
            "ProgramFiles(x86)" => Some(OsString::from(r"C:\Program Files (x86)")),
            _ => None,
        };
        let path = tool_path(Some(OsString::from(r"C:\Windows;D:\VBox\")), var).unwrap();
        let dirs: Vec<_> = std::env::split_paths(&path).collect();
        assert_eq!(dirs.iter().filter(|d| d.as_os_str() == r"D:\VBox\").count(), 1, "{dirs:?}");
        assert!(
            dirs.iter().any(|d| d.ends_with(r"Oracle\VirtualBox")) && dirs.iter().any(|d| d.ends_with(r"VMware\VMware Workstation")),
            "{dirs:?}"
        );
    }
}
