//! What an install asks of the person at the computer: a password prompt the OS raises, a licence
//! it accepts on their behalf, a setting they allow, a restart, a long wait. A UI says these
//! before the install starts (and asks consent to a licence), and again while the step that
//! raises them runs; nothing here is worded, each is a code a UI says in its own words.
//!
//! The one table of them is [`interactions`]: per OS and tool, from what the planner knows (the
//! elevation decides whether a password prompt appears at all: an unattended step never raises
//! one). [`super::plan`] hands each to the step that raises it ([`super::Step::interactions`]).

use serde::Serialize;

use super::{Dependency, Elevation};
use crate::host::{Arch, Host, HostOs, Packages};

/// One thing an install asks of the person at the computer, or does on their behalf. None at all
/// (an empty list) means it installs on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Interaction {
    /// The OS asks for an administrator's password (or approval), in a window of its own.
    AdminPassword {
        /// Which prompt.
        via: Prompt,
    },
    /// The install accepts this licence on the user's behalf: a UI asks their consent first.
    Licence {
        /// Its name, as its vendor writes it.
        name: &'static str,
        /// Where its vendor publishes it.
        url: &'static str,
    },
    /// A vendor's installer opens a window of its own that waits for the user.
    VendorWindow {
        /// The app whose window it is.
        app: &'static str,
    },
    /// The OS blocks the vendor's system extension until the user allows it in System Settings
    /// (Privacy & Security); a restart then loads it ([`Interaction::Restart`] follows).
    AllowSystemExtension {
        /// Who signs it, as their Developer ID names them.
        vendor: &'static str,
    },
    /// The computer is restarted before the tool works.
    Restart,
    /// The user signs out and back in before the tool works (a group they joined).
    Relogin,
    /// The install builds from source, which takes long.
    LongBuild {
        /// About how many minutes it takes, on a common computer.
        minutes: u16,
    },
    /// Nothing installs it unattended: its vendor offers it only from its own page (behind an
    /// account or a licence). The user downloads and installs it there, then checks again: the
    /// tool's detection reads it as present once it is installed.
    DownloadByHand {
        /// The vendor, as it names itself.
        vendor: &'static str,
        /// Its official download page.
        url: &'static str,
    },
}

/// Where a tool's vendor offers it for its user to download themselves: its name, and its
/// official download page (never a mirror).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
pub struct VendorPage {
    /// The vendor, as it names itself.
    pub name: &'static str,
    /// The official download page.
    pub url: &'static str,
}

impl VendorPage {
    /// What it asks of the user: to download the tool there themselves.
    #[must_use]
    pub fn download(self) -> Interaction {
        Interaction::DownloadByHand {
            vendor: self.name,
            url: self.url,
        }
    }
}

/// Broadcom's download portal, where VMware's desktop hypervisors are offered (behind a free
/// Broadcom account): their own pages there are [`VMWARE_FUSION_PAGE`] and
/// [`VMWARE_WORKSTATION_PAGE`].
pub const BROADCOM_DOWNLOADS: VendorPage = VendorPage {
    name: "Broadcom",
    url: "https://support.broadcom.com/group/ecx/downloads",
};

/// VMware Fusion Pro (macOS), free, on Broadcom's download portal.
pub const VMWARE_FUSION_PAGE: VendorPage = VendorPage {
    name: "Broadcom",
    url: "https://support.broadcom.com/group/ecx/productdownloads?subfamily=VMware%20Fusion&freeDownloads=true",
};

/// VMware Workstation Pro (Windows, Linux), free, on Broadcom's download portal.
pub const VMWARE_WORKSTATION_PAGE: VendorPage = VendorPage {
    name: "Broadcom",
    url: "https://support.broadcom.com/group/ecx/productdownloads?subfamily=VMware%20Workstation%20Pro&freeDownloads=true",
};

/// VMware's OVF Tool, on Broadcom's developer portal.
pub const OVFTOOL_PAGE: VendorPage = VendorPage {
    name: "Broadcom",
    url: "https://developer.broadcom.com/tools/open-virtualization-format-ovf-tool/latest",
};

/// The password prompt an administrator's step raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum Prompt {
    /// macOS's administrator prompt.
    Mac,
    /// Windows' User Account Control.
    Uac,
    /// pkexec's (polkit's) prompt, on Linux.
    Pkexec,
}

/// Docker's Subscription Service Agreement, which Docker Desktop's installs accept (macOS's
/// `install --accept-license`, winget's `--accept-license` from Docker's manifest).
const DOCKER_LICENCE: Interaction = Interaction::Licence {
    name: super::mac_image::DOCKER_LICENCE.name,
    url: super::mac_image::DOCKER_LICENCE.url,
};

/// About how long Homebrew takes to build QEMU and its libraries from source on an Intel Mac: a
/// quarter of an hour or more.
const BUILD_MINUTES: u16 = 15;

/// What installing `dep` on `host` asks of its user (`user`: the account a Linux install joins to
/// a tool's groups), its administrator's steps run `elevation`'s way: in the order they come. A
/// prompt only where one is raised: never unattended (as root, or `sudo -n`, which refuses rather
/// than asks). Empty for a tool that doesn't run there.
#[must_use]
pub fn interactions(dep: Dependency, host: &Host, elevation: Elevation, user: Option<&str>) -> Vec<Interaction> {
    if !dep.runs_on(host.os) {
        return Vec::new();
    }
    let prompt = |via| match elevation {
        Elevation::Prompt => Some(Interaction::AdminPassword { via }),
        Elevation::Unattended { .. } => None,
    };
    let found: Vec<Option<Interaction>> = match host.os {
        HostOs::Macos => macos(dep, host.arch, prompt(Prompt::Mac)),
        HostOs::Windows => windows(dep),
        HostOs::Linux => linux(dep, host.packages, user, prompt(Prompt::Pkexec)),
    };
    found.into_iter().flatten().collect()
}

/// macOS: the vendors' packages (the apps', Vagrant's, the AWS CLI's, HashiCorp's utility's)
/// install as an administrator (`admin`); Homebrew's formulae and casks as the user.
fn macos(dep: Dependency, arch: Arch, admin: Option<Interaction>) -> Vec<Option<Interaction>> {
    match dep {
        Dependency::Docker => vec![admin, Some(DOCKER_LICENCE)],
        Dependency::Vagrant | Dependency::Utm | Dependency::VmwareUtility | Dependency::Awscli => vec![admin],
        // On an Intel Mac VirtualBox's drivers are kernel extensions, which macOS blocks until
        // allowed and loads at the next restart; on Apple silicon it needs none.
        Dependency::Virtualbox => match arch {
            Arch::Amd64 => vec![
                admin,
                Some(Interaction::AllowSystemExtension {
                    vendor: super::mac_image::VIRTUALBOX.signer,
                }),
                Some(Interaction::Restart),
            ],
            Arch::Arm64 => vec![admin],
        },
        // Homebrew's formula: Homebrew has no bottles for an Intel Mac, so it builds from source
        // there.
        Dependency::Qemu => match arch {
            Arch::Amd64 => vec![Some(Interaction::LongBuild { minutes: BUILD_MINUTES })],
            Arch::Arm64 => Vec::new(),
        },
        // Homebrew's own as the user, or nothing on macOS.
        Dependency::Terraform | Dependency::Azurecli | Dependency::Gcloud | Dependency::Libvirt | Dependency::Wsl | Dependency::Hyperv => Vec::new(),
    }
}

/// Windows: every install but Terraform's (a portable zip winget unpacks for the user) is a
/// machine-wide one, which asks through UAC.
fn windows(dep: Dependency) -> Vec<Option<Interaction>> {
    let uac = Some(Interaction::AdminPassword { via: Prompt::Uac });
    match dep {
        // winget runs Docker's installer with `--accept-license` (Docker's manifest).
        Dependency::Docker => vec![uac, Some(DOCKER_LICENCE)],
        // HashiCorp's MSI asks for a restart to finish (its manifest's 3010); the Windows features
        // start at the next one.
        Dependency::Vagrant | Dependency::Wsl | Dependency::Hyperv => vec![uac, Some(Interaction::Restart)],
        Dependency::Virtualbox | Dependency::Awscli | Dependency::Azurecli | Dependency::Gcloud | Dependency::VmwareUtility => vec![uac],
        // Terraform: a portable zip winget unpacks for the user. The rest isn't on Windows.
        Dependency::Terraform | Dependency::Qemu | Dependency::Utm | Dependency::Libvirt => Vec::new(),
    }
}

/// Linux: every install is one root script (`admin`); the ones that join `user` to a tool's
/// groups (Docker's `docker`, libvirt's, `/dev/kvm`'s, Fedora's VirtualBox's `vboxusers`) apply
/// them from the next login.
fn linux(dep: Dependency, packages: Option<Packages>, user: Option<&str>, admin: Option<Interaction>) -> Vec<Option<Interaction>> {
    let dnf = packages.and_then(Packages::linux) == Some(crate::host::LinuxPackages::Dnf);
    let joins = match dep {
        Dependency::Docker | Dependency::Libvirt | Dependency::Qemu => true,
        Dependency::Virtualbox => dnf,
        _ => false,
    };
    let relogin = (joins && user.is_some_and(super::linux::scripts::valid_user)).then_some(Interaction::Relogin);
    vec![admin, relogin]
}

#[cfg(test)]
mod tests;
