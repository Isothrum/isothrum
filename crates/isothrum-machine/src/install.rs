//! Installing and removing the tools a computer runs projects with: the container engine, Vagrant,
//! Terraform, the hypervisors, the cloud CLIs and the services they need. Each tool's row
//! ([`Dependency::row`]) says where it exists and what proves it installed; its install and its
//! removal are one exhaustive match per OS over the tool and the package manager, so a tool, an OS
//! or a package manager added is a compile error until each says what it does there. Every plan is
//! built on any OS, so the tests read every row.
//!
//! A plan is a list of [`Step`]s, each one program, run in order through the [`Context`]'s runner
//! ([`run_steps`]): the OS's own trusted flows, never a password prompt of the caller's (pkexec's
//! on Linux, or `sudo -n` when nobody is there to answer one: [`Elevation`]). Why a plan can't be
//! made ([`Refusal`]) and why a step failed ([`StepFailed`]) are codes a UI says in its own words.
//! What an install leaves on record, so a clean-up removes only what it installed, is
//! [`record`]'s.

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::host::{ALL_OSES, Arch, HostOs};
use crate::providers::{Provider, ProviderRow};

pub mod brew;
pub mod exit_codes;
pub mod linux;
pub mod macos;
mod pkexec;
pub mod record;
pub mod root;
mod steps;
mod vmware_utility;
pub mod windows;

pub use steps::{NotServed, Note, Step, StepFailed, command_line, run_step, run_steps, step};

/// A tool a setup installs.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum Dependency {
    /// Docker: Docker Desktop on macOS and Windows, Docker Engine on Linux.
    Docker,
    /// HashiCorp's Vagrant.
    Vagrant,
    /// HashiCorp's Terraform.
    Terraform,
    /// Oracle's VirtualBox.
    Virtualbox,
    /// QEMU.
    Qemu,
    /// UTM (macOS).
    Utm,
    /// libvirt, with what the vagrant-libvirt plugin builds against (Linux).
    Libvirt,
    /// The AWS CLI.
    Awscli,
    /// The Azure CLI.
    Azurecli,
    /// The Google Cloud CLI.
    Gcloud,
    /// Windows Subsystem for Linux (WSL 2), enabled through its own installer.
    Wsl,
    /// HashiCorp's Vagrant VMware Utility, the service VMware's Vagrant plugin drives VMware
    /// through.
    #[serde(rename = "vmware-utility")]
    VmwareUtility,
    /// Windows' Hyper-V feature, turned on through Windows' own servicing tool (DISM).
    Hyperv,
}

/// Where a tool exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runs {
    /// On these OSes.
    On(&'static [HostOs]),
    /// Where this hypervisor does ([`ProviderRow::of`]): the one table of which OS a hypervisor
    /// belongs to.
    As(Provider),
}

/// A program and the arguments it answers with its version.
pub type Probe = (&'static str, &'static [&'static str]);

/// A tool's row of the setup's table ([`Dependency::row`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ToolRow {
    /// Where it exists.
    pub runs: Runs,
    /// The command that proves it installed, when there is one worth trusting (`wsl.exe` ships
    /// with Windows whether WSL is set up or not; UTM is an app; VMware's utility a service). A
    /// hypervisor has none here: its provider's ([`crate::providers::hypervisor_probe`]) is the one
    /// a setup and the provider's detection both read, so they never disagree.
    pub probe: Option<Probe>,
    /// The app bundle that proves it installed on macOS, also before its CLI is on `PATH`.
    pub mac_app: Option<&'static str>,
    /// Its winget package, when winget installs and removes it on Windows.
    pub winget: Option<&'static str>,
    /// Where its user installs it themselves when the setup can't (a Linux distribution without
    /// apt or dnf).
    pub own_url: Option<&'static str>,
    /// Where it runs, the CPUs its vendor ships no build for that a setup installs, each with the
    /// vendor's reason: refused there ([`Refusal::NoBuild`]), never offered.
    pub no_build: &'static [NoBuild],
}

/// An OS and CPU a tool's vendor ships no build for that a setup installs, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoBuild {
    /// The OS.
    pub os: HostOs,
    /// The CPU.
    pub arch: Arch,
    /// Why, as its vendor has it.
    pub why: &'static str,
}

/// Google Cloud CLI on Windows on Arm: Google's Windows installer (the one winget runs) puts no
/// Python on an Arm64 computer, and the CLI needs one, so its install stops ("you must have
/// Python installed", the installer's exit code 2). Google lists no Windows Arm64 build.
/// Vagrant on Linux arm64: HashiCorp ships no Linux arm64 build (its apt repository serves amd64
/// only), so apt installs the distribution's own (Debian 13: 2.3.7), older than the 2.4.0 a run
/// needs. Before 2.4.0 Vagrant reads no box's architecture: on an arm64 computer it adds a box's
/// `x86_64` build (checked: Debian's 2.3.7 on arm64 fetched `cloud-image/debian-13`'s
/// `libvirt/amd64` box), so every project's VM is of the wrong CPU, and Isothrum refuses it.
const VAGRANT_NO_BUILD: &[NoBuild] = &[NoBuild {
    os: HostOs::Linux,
    arch: Arch::Arm64,
    why: "HashiCorp ships no Linux arm64 build, and the distribution's Vagrant is older than the 2.4.0 a run needs (an older one adds a box's x86_64 build on an arm64 computer)",
}];

const GCLOUD_NO_BUILD: &[NoBuild] = &[NoBuild {
    os: HostOs::Windows,
    arch: Arch::Arm64,
    why: "Google ships no Windows on Arm build: its installer brings no Python there, which the CLI needs",
}];

impl Dependency {
    /// Every tool, in the setup's order.
    pub const ALL: [Dependency; 13] = [
        Dependency::Docker,
        Dependency::Vagrant,
        Dependency::Terraform,
        Dependency::Virtualbox,
        Dependency::Qemu,
        Dependency::Utm,
        Dependency::Libvirt,
        Dependency::Awscli,
        Dependency::Azurecli,
        Dependency::Gcloud,
        Dependency::Wsl,
        Dependency::VmwareUtility,
        Dependency::Hyperv,
    ];

    /// Its row of the setup's table.
    #[must_use]
    pub fn row(self) -> ToolRow {
        const ANY: Runs = Runs::On(&ALL_OSES);
        /// A row with no macOS app.
        const fn row(runs: Runs, probe: Option<Probe>, winget: Option<&'static str>, own_url: Option<&'static str>) -> ToolRow {
            ToolRow {
                runs,
                probe,
                mac_app: None,
                winget,
                own_url,
                no_build: &[],
            }
        }
        match self {
            Dependency::Docker => ToolRow {
                mac_app: Some("/Applications/Docker.app"),
                ..row(
                    ANY,
                    Some(("docker", &["--version"])),
                    Some("Docker.DockerDesktop"),
                    Some("https://docs.docker.com/engine/install/"),
                )
            },
            Dependency::Vagrant => ToolRow {
                no_build: VAGRANT_NO_BUILD,
                ..row(
                    ANY,
                    Some(("vagrant", &["--version"])),
                    Some("Hashicorp.Vagrant"),
                    Some("https://developer.hashicorp.com/vagrant/install"),
                )
            },
            Dependency::Terraform => row(ANY, Some(("terraform", &["version"])), Some("Hashicorp.Terraform"), None),
            Dependency::Virtualbox => ToolRow {
                mac_app: Some("/Applications/VirtualBox.app"),
                ..row(
                    Runs::As(Provider::Virtualbox),
                    None,
                    Some("Oracle.VirtualBox"),
                    Some("https://www.virtualbox.org/wiki/Linux_Downloads"),
                )
            },
            Dependency::Qemu => row(Runs::As(Provider::Qemu), None, None, Some("https://www.qemu.org/download/")),
            Dependency::Utm => ToolRow {
                mac_app: Some("/Applications/UTM.app"),
                ..row(Runs::As(Provider::Utm), None, None, None)
            },
            Dependency::Libvirt => row(Runs::As(Provider::Libvirt), None, None, Some("https://libvirt.org/")),
            Dependency::Awscli => row(ANY, Some(("aws", &["--version"])), Some("Amazon.AWSCLI"), None),
            Dependency::Azurecli => row(ANY, Some(("az", &["version"])), Some("Microsoft.AzureCLI"), None),
            Dependency::Gcloud => ToolRow {
                no_build: GCLOUD_NO_BUILD,
                ..row(ANY, Some(("gcloud", &["--version"])), Some("Google.CloudSDK"), None)
            },
            Dependency::Wsl => row(Runs::On(&[HostOs::Windows]), None, None, None),
            Dependency::VmwareUtility => row(ANY, None, None, Some("https://developer.hashicorp.com/vagrant/install/vmware")),
            Dependency::Hyperv => row(Runs::As(Provider::Hyperv), None, None, None),
        }
    }

    /// The tool a setup installs as hypervisor `provider`, where it does (the row that names it).
    #[must_use]
    pub fn of_hypervisor(provider: Provider) -> Option<Dependency> {
        Dependency::ALL.into_iter().find(|d| d.hypervisor() == Some(provider))
    }

    /// The hypervisor it serves without being it, when it does: VMware's utility serves VMware
    /// Workstation (or Fusion), whose installer it needs first (Windows' MSI refuses to run
    /// without it, exiting 1603; on Linux its service doesn't answer). A setup installs it once
    /// that hypervisor is installed, never before ([`crate::requirements::Plan::After`]).
    #[must_use]
    pub fn serves(self) -> Option<Provider> {
        match self {
            Dependency::VmwareUtility => Some(Provider::VmwareDesktop),
            _ => None,
        }
    }

    /// The hypervisor it is, when it is one.
    #[must_use]
    pub fn hypervisor(self) -> Option<Provider> {
        match self.row().runs {
            Runs::As(provider) => Some(provider),
            Runs::On(_) => None,
        }
    }

    /// Why its vendor ships no build of it for `os` on `arch` that a setup installs, when it
    /// doesn't ([`ToolRow::no_build`]).
    #[must_use]
    pub fn no_build(self, os: HostOs, arch: Arch) -> Option<&'static str> {
        self.row().no_build.iter().find(|n| n.os == os && n.arch == arch).map(|n| n.why)
    }

    /// Whether it exists on `os`: a hypervisor where its provider's row says, the rest by its own
    /// row. An install or a removal elsewhere is refused ([`Refusal::NotHere`]).
    #[must_use]
    pub fn runs_on(self, os: HostOs) -> bool {
        match self.row().runs {
            Runs::On(oses) => oses.contains(&os),
            Runs::As(provider) => ProviderRow::of(provider).oses.contains(&os),
        }
    }
}

/// The step that installs the Vagrant plugin `plugin` (for this account, no administrator), when
/// `plugin` names one: only ever a `vagrant-*` / `vagrant_*` identifier, never an option or a path.
#[must_use]
pub fn vagrant_plugin(plugin: &str) -> Option<Step> {
    let named = (plugin.starts_with("vagrant-") || plugin.starts_with("vagrant_"))
        && plugin.len() <= 64
        && plugin.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    named.then(|| step("vagrant", &["plugin", "install", plugin]))
}

/// How a step that needs an administrator is elevated (Linux). With someone there to answer,
/// pkexec's password prompt; with nobody (a script, a machine set up unattended), no prompt may
/// be raised: as is when the process is root already, else through `sudo -n`, refused at once
/// rather than waiting on a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// pkexec's password prompt.
    Prompt,
    /// Unattended, `root` when the process is root already.
    Unattended {
        /// Whether the process runs as root.
        root: bool,
    },
}

impl Elevation {
    /// `program` with `args`, run as root this way.
    #[must_use]
    pub fn as_root(self, program: &str, args: &[String]) -> (String, Vec<String>) {
        let all = |prefix: &[&str]| {
            prefix
                .iter()
                .map(ToString::to_string)
                .chain(std::iter::once(program.to_owned()))
                .chain(args.iter().cloned())
                .collect()
        };
        match self {
            Elevation::Prompt => ("pkexec".into(), all(&[])),
            Elevation::Unattended { root: true } => (program.to_owned(), args.to_vec()),
            Elevation::Unattended { root: false } => ("sudo".into(), all(&["-n"])),
        }
    }
}

/// Why a plan can't be made here. A UI says each in its own words.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The tool doesn't run on this OS: there is nothing of it to install or remove here.
    NotHere {
        /// The tool.
        dependency: Dependency,
        /// The OS.
        os: HostOs,
    },
    /// A Linux distribution with neither apt nor dnf: its user installs the tool themselves.
    WithoutPackages {
        /// The tool.
        dependency: Dependency,
    },
    /// A tool installed through apt only for now, on a dnf distribution.
    AptOnly {
        /// The tool.
        dependency: Dependency,
    },
    /// Homebrew isn't installed, which a macOS install goes through.
    NoHomebrew,
    /// Homebrew isn't installed, which this removal on macOS goes through.
    HomebrewNeeded,
    /// VirtualBox's removal on macOS without its installer image, whose uninstaller it runs
    /// ([`macos::virtualbox_image`]).
    NoVirtualboxImage,
    /// A Windows feature, which its user turns off in Windows Features.
    WindowsFeature {
        /// The tool.
        dependency: Dependency,
    },
    /// Windows on Arm, which this hypervisor (or the one this tool serves) isn't built for.
    NotOnWindowsOnArm {
        /// The tool.
        dependency: Dependency,
        /// The hypervisor.
        provider: Provider,
    },
    /// Its vendor ships no build for this OS and CPU that a setup installs ([`ToolRow::no_build`]).
    NoBuild {
        /// The tool.
        dependency: Dependency,
        /// The OS.
        os: HostOs,
        /// The CPU.
        arch: Arch,
        /// Why, as its vendor has it.
        why: &'static str,
    },
    /// The private file a root step's proxy settings go through couldn't be written: what the OS
    /// said.
    PrivateFile(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::NotHere { dependency, os } => write!(f, "{dependency:?} doesn't run on {}", crate::host::os_name(*os)),
            Refusal::WithoutPackages { dependency } => write!(f, "{dependency:?}: this distribution has neither apt nor dnf"),
            Refusal::AptOnly { dependency } => write!(f, "{dependency:?} installs through apt only"),
            Refusal::NoHomebrew | Refusal::HomebrewNeeded => f.write_str("Homebrew isn't installed"),
            Refusal::WindowsFeature { dependency } => write!(f, "{dependency:?} is turned off in Windows Features"),
            Refusal::NotOnWindowsOnArm { dependency, .. } => write!(f, "{dependency:?} doesn't run on Windows on Arm"),
            Refusal::NoBuild { dependency, os, arch, why } => write!(f, "{dependency:?} isn't offered on {} {arch:?}: {why}", crate::host::os_name(*os)),
            Refusal::NoVirtualboxImage => f.write_str("VirtualBox's installer image is needed to run its uninstaller"),
            Refusal::PrivateFile(why) => write!(f, "a root step's private file couldn't be written: {why}"),
        }
    }
}

impl std::error::Error for Refusal {}

/// The install of `dep` on `host`, whatever its OS: macOS's Homebrew formulae and vendors'
/// installers, Windows' winget packages and elevated installers, or Linux's root script for the
/// user `user` (joined to the groups a tool's devices need), its root steps run `elevation`'s way
/// with this process's proxy ([`linux::proxy_assignments`]). The one call a caller plans an
/// install with: which OS's plan applies is the host's, never the OS the crate is built for.
///
/// # Errors
///
/// [`Refusal`]: why it can't be installed there ([`Refusal::NotHere`] first, before the package
/// manager is looked at).
pub fn plan(dep: Dependency, host: &crate::host::Host, user: Option<&str>, elevation: Elevation) -> Result<Vec<Step>, Refusal> {
    if !dep.runs_on(host.os) {
        return Err(Refusal::NotHere { dependency: dep, os: host.os });
    }
    match host.os {
        HostOs::Macos => macos::install(dep, host.packages, host.arch),
        HostOs::Windows => windows::install(dep, host.arch),
        HostOs::Linux => {
            let root = linux::Root {
                elevation,
                proxy: linux::proxy_assignments(|name| std::env::var(name).ok()),
            };
            linux::install(dep, host.packages, host.arch, user, &root)
        }
    }
}

/// Whether `dep` is installed on the context's host; `None` when that can't be told (WSL, Hyper-V).
/// Its macOS app bundle, else its version probe ([`ToolRow::probe`]); VMware's utility, a service,
/// as [`crate::providers::vmware_utility`] finds it. A hypervisor with the programs its VMs need
/// beside it ([`crate::providers::helpers()`]), as its provider's detection reads it: one without
/// them is installed again, which brings them.
pub async fn present(cx: &Context, dep: Dependency) -> Option<bool> {
    if dep == Dependency::VmwareUtility {
        return Some(crate::providers::vmware_utility(&cx.host).await != crate::providers::UtilityState::Missing);
    }
    let row = dep.row();
    let mac_app = row.mac_app.filter(|_| cx.host.os == HostOs::Macos);
    if mac_app.is_some_and(|a| std::path::Path::new(a).exists()) {
        return Some(true);
    }
    let probe = dep
        .hypervisor()
        .map_or(row.probe, |provider| crate::providers::hypervisor_probe(provider, cx.host.arch));
    let found = match probe {
        Some((program, args)) => cx.read_ok(program, args).await.is_ok(),
        None if mac_app.is_some() => false,
        None => return None,
    };
    match dep.hypervisor() {
        Some(provider) if found => Some(crate::providers::helper::missing(cx, provider).await.is_none()),
        _ => Some(found),
    }
}

#[cfg(test)]
mod tests;
