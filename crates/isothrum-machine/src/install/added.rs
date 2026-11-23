//! What an install added, and nothing more: before a plan runs, which of its items are there
//! already ([`before`]); after it, which it added ([`Before::installed`]). An item is what a
//! removal takes away again ([`Item`]): a Linux distribution's package, or the tool itself where
//! it comes in one piece. An item that was there before the install is never among what it added,
//! so a clean-up that removes only what an install added ([`super::record`]) never removes what
//! someone else installed: a QEMU installed by hand stays when an install only added the programs
//! its VMs need beside it ([`crate::providers::helpers`]).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::Dependency;
use crate::context::Context;
use crate::host::{Host, HostOs, LinuxPackages};

/// What an install brings and a removal takes away again.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Item {
    /// A package of the Linux distribution's package manager (apt or dnf), by its name. In a
    /// tool's list ([`items`]) a name ending in `*` stands for every package it starts.
    Package(String),
    /// The tool itself, in one piece as its vendor's installer, Homebrew or winget brings it (an
    /// app, a formula, a cask, a winget package, an MSI, the AWS CLI's own folder on Linux), there
    /// as [`super::present`] finds it, its helpers aside.
    Tool(Dependency),
}

/// The packages `dep`'s install through `packages` brings that its removal takes away, `name*`
/// for every package that name starts: the tool's own and the programs its VMs need beside it
/// ([`crate::providers::helpers`]); never what other tools share (the compiler, Ruby's headers,
/// a kernel's headers), nor the vendor's repository. The install's metapackages (`qemu-system`,
/// `libvirt-daemon-system`) pull in the packages with the binaries a check looks for
/// (`qemu-system-x86_64`, `virsh`): those are its too, or the tool would still read installed.
/// Empty where the tool isn't a package here (the AWS CLI's folder; a tool apt alone installs on
/// a dnf distribution).
#[must_use]
pub fn packages(dep: Dependency, packages: LinuxPackages) -> Vec<String> {
    let own: &[&str] = match (dep, packages) {
        (Dependency::Docker, LinuxPackages::Apt) => &[
            "docker-ce",
            "docker-ce-cli",
            "containerd.io",
            "docker-buildx-plugin",
            "docker-compose-plugin",
            // The distribution's own, where Docker's repository serves no packages.
            "docker.io",
            "docker-compose-v2",
            "docker-compose",
        ],
        (Dependency::Docker, LinuxPackages::Dnf) => &["docker-ce", "docker-ce-cli", "containerd.io", "docker-buildx-plugin", "docker-compose-plugin"],
        (Dependency::Vagrant, _) => &["vagrant"],
        (Dependency::Terraform, _) => &["terraform"],
        (Dependency::Virtualbox, LinuxPackages::Apt) => &["virtualbox*"],
        (Dependency::Virtualbox, LinuxPackages::Dnf) => &["VirtualBox", "akmod-VirtualBox", "kmod-VirtualBox*"],
        (Dependency::Qemu, LinuxPackages::Apt) => &["qemu-system*", "qemu-utils"],
        (Dependency::Qemu, LinuxPackages::Dnf) => &["qemu-kvm", "qemu-img"],
        (Dependency::Libvirt, LinuxPackages::Apt) => &["libvirt-daemon*", "libvirt-clients", "virt-manager"],
        (Dependency::Libvirt, LinuxPackages::Dnf) => &["libvirt", "libvirt-daemon-kvm", "virt-install", "virt-manager", "libvirt-devel"],
        (Dependency::Azurecli, LinuxPackages::Apt) => &["azure-cli"],
        (Dependency::Gcloud, LinuxPackages::Apt) => &["google-cloud-cli"],
        // With `net-tools`, whose `netstat` vagrant-vmware-desktop reads the routing table with
        // ([`crate::providers::needs::vmware_utility`]): its install brings it.
        (Dependency::VmwareUtility, LinuxPackages::Apt) => &["vagrant-vmware-utility", "net-tools"],
        (Dependency::Awscli | Dependency::Utm | Dependency::Wsl | Dependency::Hyperv, _)
        | (Dependency::Azurecli | Dependency::Gcloud | Dependency::VmwareUtility, LinuxPackages::Dnf) => &[],
    };
    let helpers = dep
        .hypervisor()
        .map_or(&[][..], |provider| crate::providers::helpers(provider, HostOs::Linux))
        .iter()
        .map(|helper| helper.package(packages));
    own.iter().copied().chain(helpers).map(str::to_owned).collect()
}

/// What `dep`'s install on `host` may add: its packages ([`packages`]) on a Linux distribution
/// with apt or dnf, else the tool itself ([`Item::Tool`]).
#[must_use]
pub fn items(dep: Dependency, host: &Host) -> Vec<Item> {
    let linux = host.packages.and_then(crate::host::Packages::linux).filter(|_| host.os == HostOs::Linux);
    match linux {
        Some(manager) if dep != Dependency::Awscli => packages(dep, manager).into_iter().map(Item::Package).collect(),
        _ => vec![Item::Tool(dep)],
    }
}

/// Whether the package `name` is one `pattern` stands for: the same name, or one it starts
/// when it ends in `*`.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
    pattern.strip_suffix('*').map_or(pattern == name, |prefix| name.starts_with(prefix))
}

/// The packages the context's Linux package manager has installed, by name: dpkg's (apt) whose
/// state is anything but "not installed" or "config files only", or rpm's (dnf). `None` when
/// they can't be read.
async fn installed_packages(cx: &Context, manager: LinuxPackages) -> Option<BTreeSet<String>> {
    match manager {
        LinuxPackages::Apt => {
            let out = cx.read_ok("dpkg-query", &["-W", "-f", "${db:Status-Abbrev}\t${Package}\n"]).await.ok()?;
            Some(
                out.lines()
                    .filter_map(|line| line.split_once('\t'))
                    .filter(|(state, _)| !matches!(state.chars().nth(1), None | Some('n' | 'c')))
                    .map(|(_, name)| name.trim().to_owned())
                    .collect(),
            )
        }
        LinuxPackages::Dnf => {
            let out = cx.read_ok("rpm", &["-qa", "--qf", "%{NAME}\n"]).await.ok()?;
            Some(out.lines().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned).collect())
        }
    }
}

/// Which of `dep`'s items ([`items`]) are on the context's host now, each package by its own
/// name. `None` when that can't be told (the package manager's list unread, WSL, Hyper-V).
pub async fn there(cx: &Context, dep: Dependency) -> Option<BTreeSet<Item>> {
    let mut found = BTreeSet::new();
    let mut installed = None;
    for item in items(dep, &cx.host) {
        match item {
            Item::Package(pattern) => {
                if installed.is_none() {
                    let manager = cx.host.packages.and_then(crate::host::Packages::linux)?;
                    installed = Some(installed_packages(cx, manager).await?);
                }
                let names = installed.iter().flatten().filter(|name| matches(&pattern, name));
                found.extend(names.map(|name| Item::Package(name.clone())));
            }
            Item::Tool(tool) => {
                if super::tool_found(cx, tool).await? {
                    found.insert(Item::Tool(tool));
                }
            }
        }
    }
    Some(found)
}

/// Which of a tool's items were there before its install ran ([`before`]).
#[derive(Debug, Clone)]
pub struct Before {
    dependency: Dependency,
    there: Option<BTreeSet<Item>>,
}

/// What of `dep`'s install is on the context's host before it runs: read before running its
/// plan, then [`Before::installed`] after it.
pub async fn before(cx: &Context, dep: Dependency) -> Before {
    Before {
        dependency: dep,
        there: there(cx, dep).await,
    }
}

/// What an install added: what a clean-up may remove again, and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Installed {
    /// The items missing before the install and there after it.
    pub added: Vec<Item>,
    /// The tool itself where it was missing before and isn't found yet: its installer left open
    /// for its user to finish (a macOS cask), or `PATH` not read again yet. It counts as added
    /// once a later check finds it ([`super::record::settle`]).
    pub waiting: Vec<Item>,
}

impl Installed {
    /// What an install that added `added` and left `waiting` reports.
    #[must_use]
    pub fn new(added: Vec<Item>, waiting: Vec<Item>) -> Installed {
        Installed { added, waiting }
    }

    /// Whether it added nothing, nor left anything waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.waiting.is_empty()
    }
}

impl Before {
    /// What the install added, read on the context's host after it ran: the items there now that
    /// weren't before. Nothing when either can't be told: a record never claims what it can't
    /// prove the install added.
    pub async fn installed(self, cx: &Context) -> Installed {
        let Some(before) = self.there else {
            return Installed::default();
        };
        let Some(after) = there(cx, self.dependency).await else {
            return Installed::default();
        };
        let added = after.difference(&before).cloned().collect();
        let waiting = items(self.dependency, &cx.host)
            .into_iter()
            .filter(|item| matches!(item, Item::Tool(_)) && !before.contains(item) && !after.contains(item))
            .collect();
        Installed { added, waiting }
    }
}

#[cfg(test)]
mod tests;
