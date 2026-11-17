//! A guided setup of this computer: what to install, start or restart, in which order, for the
//! kinds of project wanted (containers, VMs), with which engine and which hypervisor. The one
//! place these rules live:
//! - [`options`] works out, from what the machine check found ([`Machine`]), what each choice a
//!   setup offers takes here: a UI renders those actions and runs each, never working them out
//!   itself;
//! - [`plan()`] puts them in order for a whole setup, from what is [`Wanted`];
//! - [`walk`] runs that plan step by step through the caller's own runner of each step, the
//!   machine checked again after each (an install changes what comes next).
//!
//! Why something is passed over or left alone is a code ([`PassedOver`], [`LeftAlone`]) a UI says
//! in its own words.

mod plan;
mod walk;

pub use plan::{LeftAlone, PassedOver, Plan, SetupPath, Step, Wanted, left_alone, plan};
pub use walk::{Event, OnFailure, Steps, Walked, walk};

use serde::{Deserialize, Serialize};

use crate::check::Check;
use crate::docker::{self, Engine};
use crate::host::{Host, HostOs};
use crate::install::Dependency;
use crate::providers::{self, Need, Provider, ProviderRow, ProviderStatus, Unavailable};
use crate::wsl::WslState;

/// One thing a setup does, or leaves to its user, on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "SetupAction"))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Action {
    /// Installs `dependency` ([`crate::install`]): the container engine, WSL 2, Vagrant, a
    /// hypervisor, the Vagrant VMware Utility.
    Install {
        /// The tool to install.
        dependency: Dependency,
    },
    /// Installs the Vagrant plugin `plugin` ([`crate::install::vagrant_plugin`]).
    Plugin {
        /// The plugin's name (`vagrant-qemu`, …).
        plugin: String,
    },
    /// Starts the installed engine `engine`.
    StartEngine {
        /// The engine to start.
        engine: Engine,
    },
    /// Points Docker at `engine`, which runs already ([`docker::use_engine`]).
    UseEngine {
        /// The engine Docker is to use.
        engine: Engine,
    },
    /// Restarts the computer, which finishes turning `feature` on.
    Restart {
        /// The feature the restart starts.
        feature: WindowsFeature,
    },
    /// Signs out and back in: a group the account was added to (Linux's `docker`, `libvirt` and
    /// `kvm`, the libvirt and QEMU installs' too) applies from the next login.
    Relogin,
    /// Not something a setup does here: its user gets it (`dependency`, when it is one a setup
    /// installs elsewhere; from `url`, when there is one), then checks the machine again.
    Manual {
        /// Why it is the user's to get.
        reason: ManualReason,
        /// The tool, when it is one a setup installs elsewhere.
        dependency: Option<Dependency>,
        /// Where to get it, when there is a page for it.
        url: Option<String>,
    },
    /// Installed, but it can't run here, and nothing a setup installs mends that: `reason`.
    Blocked {
        /// Why it can't run here, as a code a UI says in its own words.
        reason: Unavailable,
    },
}

impl Action {
    /// Whether it ends a setup pass: what follows waits for the restart or the new login. A
    /// hypervisor blocked until the computer restarts (VirtualBox's driver, built for the newer
    /// kernel its install brought) ends it too, as a restart.
    #[must_use]
    pub fn ends_pass(&self) -> bool {
        matches!(
            self,
            Action::Restart { .. }
                | Action::Relogin
                | Action::Blocked {
                    reason: Unavailable::KernelRestart { .. }
                }
        )
    }

    /// Its kind, as an event and a log's name say it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Install { .. } => "install",
            Action::Plugin { .. } => "plugin",
            Action::StartEngine { .. } => "start",
            Action::UseEngine { .. } => "use",
            Action::Restart { .. } => "restart",
            Action::Relogin => "relogin",
            Action::Manual { .. } => "manual",
            Action::Blocked { .. } => "blocked",
        }
    }
}

/// A Windows feature that starts only after a restart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum WindowsFeature {
    /// WSL 2: the Virtual Machine Platform and the Windows Subsystem for Linux.
    Wsl,
    /// Hyper-V.
    Hyperv,
}

/// Why something is the user's to get.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ManualReason {
    /// From its vendor (behind a login or a licence): a UI links out.
    Vendor,
    /// A setup doesn't install on this Linux distribution yet (it installs through apt or dnf).
    Distribution,
}

/// What each choice of a setup takes on this computer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "SetupOptions"))]
#[serde(rename_all = "camelCase")]
pub struct Options {
    /// What turns WSL 2 on (Windows): nothing once it is ready, or elsewhere.
    pub wsl: Vec<Action>,
    /// The engine a setup installs in one click on this OS.
    pub recommended_engine: Engine,
    /// The engine a setup works on until its user picks one: the one in use, else the
    /// recommended one.
    pub engine: Engine,
    /// The engines offered on this OS, in the order shown, each with what makes it the one Docker
    /// uses here (nothing: it is).
    pub engines: Vec<EngineOption>,
    /// The hypervisor a setup works on until its user picks one: an installed one that can run
    /// VMs, else an installed one, else the first a setup installs, else the first.
    pub hypervisor: Option<Provider>,
    /// The local hypervisors offered here, in the machine check's order.
    pub hypervisors: Vec<HypervisorOption>,
}

impl Options {
    /// None yet: what a report holds until its options are worked out.
    #[must_use]
    pub fn empty() -> Options {
        Options {
            wsl: Vec::new(),
            recommended_engine: Engine::Native,
            engine: Engine::Native,
            engines: Vec::new(),
            hypervisor: None,
            hypervisors: Vec::new(),
        }
    }
}

/// A container engine, and what makes it the one Docker uses here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EngineOption {
    /// Which engine it is (Docker Desktop, Colima, the Linux daemon…).
    pub engine: Engine,
    /// What makes it the one Docker uses here, in order: nothing when it is.
    pub actions: Vec<Action>,
}

/// A local hypervisor: what gets it here (`get`: nothing once it is installed and can run), then
/// what Vagrant needs to drive it (`vagrant`: Vagrant, its plugin, VMware's utility, in order).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HypervisorOption {
    /// Which hypervisor, as the Vagrant provider that drives it.
    pub provider: Provider,
    /// What installs it or turns it on, in order: nothing when it is ready.
    pub get: Vec<Action>,
    /// What Vagrant needs to drive it: Vagrant, its plugin, VMware's utility, in order.
    pub vagrant: Vec<Action>,
}

/// What a setup reads of the machine check ([`Check`]): the computer, its package manager, Docker
/// and its engines, WSL 2, the Vagrant providers and the groups pending a login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// The computer ([`providers::login_groups`] reads which group opens `/dev/kvm` on it).
    pub host: Host,
    /// Whether the package manager a setup installs through is there.
    pub packages_installed: bool,
    /// Docker and its engines.
    pub docker: DockerFound,
    /// WSL 2's state (Windows), when it was checked.
    pub wsl: Option<WslState>,
    /// Every Vagrant provider offered here.
    pub providers: Vec<ProviderStatus>,
    /// The groups this account joined since logging in (Linux).
    pub groups_pending: Vec<String>,
}

/// Docker and its engines, as the machine check found them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DockerFound {
    /// Whether the Docker CLI is installed.
    pub installed: bool,
    /// Whether the daemon the Docker CLI uses answers this account.
    pub running: bool,
    /// Whether that daemon runs but refused this account.
    pub denied: bool,
    /// The engine behind that daemon, when it answered.
    pub engine: Option<Engine>,
    /// The engines found running.
    pub engines_running: Vec<Engine>,
}

impl DockerFound {
    /// Whether Docker answers: its CLI is installed and its daemon runs for this account.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.installed && self.running
    }
}

impl Machine {
    /// What `check` found on `host`.
    #[must_use]
    pub fn of(check: &Check, host: Host) -> Machine {
        Machine {
            host,
            packages_installed: check.packages.installed,
            docker: DockerFound {
                installed: check.docker.installed,
                running: check.daemon.running(),
                denied: check.daemon.denied(),
                engine: check.daemon.engine,
                engines_running: docker::running_of(&check.engines),
            },
            wsl: check.wsl.as_ref().map(|w| w.state),
            providers: check.providers.clone(),
            groups_pending: check.groups_pending.clone(),
        }
    }

    /// Its local hypervisors.
    fn local(&self) -> impl Iterator<Item = &ProviderStatus> {
        self.providers.iter().filter(|p| !p.remote)
    }
}

/// The engines a setup offers on `os`, in the order it shows them.
#[must_use]
pub fn engines_on(os: HostOs) -> &'static [Engine] {
    match os {
        HostOs::Macos => &[Engine::DockerDesktop, Engine::Orbstack, Engine::Colima],
        HostOs::Windows => &[Engine::DockerDesktop],
        HostOs::Linux => &[Engine::DockerDesktop, Engine::Native, Engine::Colima],
    }
}

/// The engine a setup installs in one click on `os`: Docker Desktop on macOS and Windows, Docker
/// Engine on Linux.
#[must_use]
pub fn recommended_engine(os: HostOs) -> Engine {
    match os {
        HostOs::Macos | HostOs::Windows => Engine::DockerDesktop,
        HostOs::Linux => Engine::Native,
    }
}

/// The hypervisor a setup installs as `provider` on `os`, when it installs it.
#[must_use]
pub fn installable(provider: Provider, os: HostOs) -> Option<Dependency> {
    Dependency::of_hypervisor(provider).filter(|d| d.runs_on(os))
}

/// An installed hypervisor that still can't run VMs once everything Vagrant needs is in (no
/// `/dev/kvm`, not in the kvm group, a stuck service): its `reason` says why.
#[must_use]
pub fn cant_run(p: &ProviderStatus) -> bool {
    p.hypervisor == Some(true) && !p.available && p.reason.is_some() && p.needs.is_empty()
}

/// Installing `dependency` here: its install, or on a Linux distribution without a package
/// manager a setup installs through, its user's to do.
fn install(m: &Machine, dependency: Dependency) -> Action {
    if m.host.os == HostOs::Linux && !m.packages_installed {
        return Action::Manual {
            reason: ManualReason::Distribution,
            dependency: Some(dependency),
            url: dependency.row().own_url.map(str::to_owned),
        };
    }
    Action::Install { dependency }
}

/// What turns WSL 2 on: nothing off Windows, once it is ready, or once a Docker engine runs (proof
/// enough, whatever the check could tell); the restart once it waits for one; else its install
/// (also when the check couldn't tell).
fn wsl(m: &Machine) -> Vec<Action> {
    if m.host.os != HostOs::Windows || m.docker.ready() {
        return Vec::new();
    }
    match m.wsl.unwrap_or(WslState::Unknown) {
        WslState::Ready => Vec::new(),
        WslState::NeedsReboot => vec![Action::Restart { feature: WindowsFeature::Wsl }],
        WslState::Missing | WslState::Unknown => vec![install(m, Dependency::Wsl)],
    }
}

/// What makes `engine` the one Docker uses: nothing when it is; switching to it when it runs; a
/// new login when the daemon refuses this account (Linux's docker group); starting it when Docker
/// is installed but doesn't answer; installing it when it is the recommended one and there is no
/// Docker; else its vendor's site.
fn engine_actions(m: &Machine, engine: Engine) -> Vec<Action> {
    let action = if m.docker.ready() && m.docker.engine == Some(engine) {
        return Vec::new();
    } else if m.docker.engines_running.contains(&engine) {
        Action::UseEngine { engine }
    } else if m.docker.denied && engine == Engine::Native {
        Action::Relogin
    } else if m.docker.installed && !m.docker.ready() {
        Action::StartEngine { engine }
    } else if engine == recommended_engine(m.host.os) && !m.docker.installed {
        install(m, Dependency::Docker)
    } else {
        Action::Manual {
            reason: ManualReason::Vendor,
            dependency: None,
            url: Some(engine.url().to_owned()),
        }
    };
    vec![action]
}

/// What gets hypervisor `p` here: nothing once it is installed (a new login when the account
/// joined its groups since logging in, libvirt's or `/dev/kvm`'s; else why it still can't run);
/// the restart Hyper-V waits for once a setup turned it on (`hyperv_restart`); its install; else
/// its vendor's site.
fn hypervisor_get(m: &Machine, p: &ProviderStatus, hyperv_restart: bool) -> Vec<Action> {
    // Nothing installed mends it (KVM holding the CPU's virtualization, for VirtualBox): never
    // installed, nor readied, for nothing.
    if let Some(reason @ Unavailable::KvmHoldsVirtualization) = &p.reason {
        return vec![Action::Blocked { reason: reason.clone() }];
    }
    if p.hypervisor == Some(true) {
        // The groups its install added the account to (libvirt's, /dev/kvm's) apply from the
        // next login: a setup asks for it, as it does for the docker group.
        let groups = providers::login_groups(p.provider, &m.host);
        if m.groups_pending.iter().any(|g| groups.contains(g)) {
            return vec![Action::Relogin];
        }
        return match &p.reason {
            Some(reason) if cant_run(p) => vec![Action::Blocked { reason: reason.clone() }],
            _ => Vec::new(),
        };
    }
    if p.provider == Provider::Hyperv && hyperv_restart {
        return vec![Action::Restart {
            feature: WindowsFeature::Hyperv,
        }];
    }
    vec![match installable(p.provider, m.host.os) {
        Some(dependency) => install(m, dependency),
        None => Action::Manual {
            reason: ManualReason::Vendor,
            dependency: None,
            url: ProviderRow::of(p.provider).vendor.map(str::to_owned),
        },
    }]
}

/// What Vagrant needs to drive `p` ([`ProviderStatus::needs`]), each with the item it is about:
/// `vagrant`, `plugin:<name>`, `vmware-utility`.
fn vagrant_steps(m: &Machine, p: &ProviderStatus) -> Vec<(String, Action)> {
    p.needs
        .iter()
        .filter_map(|need| {
            Some(match need {
                Need::Vagrant => ("vagrant".to_owned(), install(m, Dependency::Vagrant)),
                Need::Plugin => {
                    let plugin = p.plugin.clone()?;
                    (format!("plugin:{plugin}"), Action::Plugin { plugin })
                }
                Need::VmwareUtility => ("vmware-utility".to_owned(), install(m, Dependency::VmwareUtility)),
            })
        })
        .collect()
}

/// What each choice takes on machine `m`; `hyperv_restart`: a setup turned Hyper-V on in this
/// boot, so it waits for a restart.
#[must_use]
pub fn options(m: &Machine, hyperv_restart: bool) -> Options {
    let offered = engines_on(m.host.os);
    let recommended = recommended_engine(m.host.os);
    let engine = match m.docker.engine {
        Some(e) if offered.contains(&e) => e,
        Some(_) => offered.first().copied().unwrap_or(recommended),
        None => recommended,
    };
    let hypervisors: Vec<HypervisorOption> = m
        .local()
        .map(|p| HypervisorOption {
            provider: p.provider,
            get: hypervisor_get(m, p, hyperv_restart),
            vagrant: vagrant_steps(m, p).into_iter().map(|(_, action)| action).collect(),
        })
        .collect();
    let installed = |o: &&HypervisorOption| m.local().any(|p| p.provider == o.provider && p.hypervisor == Some(true));
    let hypervisor = hypervisors
        .iter()
        .find(|o| installed(o) && o.get.is_empty())
        .or_else(|| hypervisors.iter().find(installed))
        .or_else(|| hypervisors.iter().find(|o| matches!(o.get.first(), Some(Action::Install { .. }))))
        .or_else(|| hypervisors.first())
        .map(|o| o.provider);
    Options {
        wsl: wsl(m),
        recommended_engine: recommended,
        engine,
        engines: offered
            .iter()
            .map(|&e| EngineOption {
                engine: e,
                actions: engine_actions(m, e),
            })
            .collect(),
        hypervisor,
        hypervisors,
    }
}

#[cfg(test)]
mod tests;
