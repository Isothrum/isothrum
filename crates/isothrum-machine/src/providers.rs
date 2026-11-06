//! The Vagrant providers (hypervisors) this computer can drive, and detecting which are usable.
//! Vagrant abstracts the hypervisor; we detect what is installed here and let `vagrant up
//! --provider` do the rest. Ported from the launcher's `runtime/providers`; the Windows
//! "may this account manage Hyper-V" refinement and the VirtualBox-wedged check are left for a
//! later version, so detection here is the hypervisor and its plugin, not the account's rights.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::ProcessRunner;
use serde::{Deserialize, Serialize};

use crate::exec;

/// A Vagrant provider id (the value given to `vagrant up --provider`). The remote providers and
/// the clouds share the enum but are never detected here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provider {
    /// Oracle VirtualBox (built into Vagrant).
    Virtualbox,
    /// VMware Workstation or Fusion (plugin `vagrant-vmware-desktop`).
    VmwareDesktop,
    /// Microsoft Hyper-V (built into Vagrant, Windows only).
    Hyperv,
    /// Parallels Desktop (plugin `vagrant-parallels`, macOS only).
    Parallels,
    /// libvirt / KVM (plugin `vagrant-libvirt`, Linux only).
    Libvirt,
    /// QEMU (plugin `vagrant-qemu`).
    Qemu,
    /// UTM, a QEMU front end for macOS (plugin `vagrant_utm`).
    Utm,
    /// A remote VMware ESXi host (plugin `vagrant-vmware-esxi`).
    VmwareEsxi,
    /// A remote Proxmox VE host.
    Proxmox,
    /// The player's AWS account (Terraform, never a local Vagrant provider).
    Aws,
    /// The player's Azure subscription (Terraform).
    Azure,
    /// The player's Google Cloud project (Terraform).
    Gcp,
    /// The player's DigitalOcean account (Terraform).
    #[serde(rename = "digitalocean")]
    DigitalOcean,
    /// The player's Linode account (Terraform).
    Linode,
    /// The player's Oracle Cloud tenancy (Terraform).
    Oci,
}

impl Provider {
    /// Every provider that is a local or remote Vagrant hypervisor (the clouds are left out: they
    /// are never detected on this machine).
    pub const ALL: [Provider; 9] = [
        Provider::Virtualbox,
        Provider::VmwareDesktop,
        Provider::Hyperv,
        Provider::Parallels,
        Provider::Libvirt,
        Provider::Qemu,
        Provider::Utm,
        Provider::VmwareEsxi,
        Provider::Proxmox,
    ];

    /// The id Vagrant knows the provider by.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Provider::Virtualbox => "virtualbox",
            Provider::VmwareDesktop => "vmware_desktop",
            Provider::Hyperv => "hyperv",
            Provider::Parallels => "parallels",
            Provider::Libvirt => "libvirt",
            Provider::Qemu => "qemu",
            Provider::Utm => "utm",
            Provider::VmwareEsxi => "vmware_esxi",
            Provider::Proxmox => "proxmox",
            Provider::Aws => "aws",
            Provider::Azure => "azure",
            Provider::Gcp => "gcp",
            Provider::DigitalOcean => "digitalocean",
            Provider::Linode => "linode",
            Provider::Oci => "oci",
        }
    }

    /// Whether the provider runs VMs on another host (so it needs connection settings, not a local
    /// hypervisor).
    #[must_use]
    pub fn is_remote(self) -> bool {
        matches!(
            self,
            Provider::VmwareEsxi | Provider::Proxmox | Provider::Aws | Provider::Azure | Provider::Gcp | Provider::DigitalOcean | Provider::Linode | Provider::Oci
        )
    }

    /// Whether the provider is a cloud account billed to the player, as opposed to their own server.
    #[must_use]
    pub fn is_cloud(self) -> bool {
        self.is_remote() && !matches!(self, Provider::VmwareEsxi | Provider::Proxmox)
    }

    /// Whether this provider can run on the current OS at all: local hypervisors are
    /// platform-specific, remote ones apply anywhere.
    fn applicable(self) -> bool {
        use std::env::consts::{ARCH, OS};
        match self {
            Provider::Virtualbox | Provider::VmwareDesktop if OS == "windows" && ARCH == "aarch64" => false,
            Provider::Hyperv => OS == "windows",
            Provider::Parallels | Provider::Utm => OS == "macos",
            Provider::Libvirt => OS == "linux",
            _ => true,
        }
    }

    /// The Vagrant plugin the provider needs, if it is not built in.
    fn plugin(self) -> Option<&'static str> {
        match self {
            Provider::Virtualbox
            | Provider::Hyperv
            | Provider::Aws
            | Provider::Azure
            | Provider::Gcp
            | Provider::DigitalOcean
            | Provider::Linode
            | Provider::Oci => None,
            Provider::VmwareDesktop => Some("vagrant-vmware-desktop"),
            Provider::Parallels => Some("vagrant-parallels"),
            Provider::Libvirt => Some("vagrant-libvirt"),
            Provider::Qemu => Some("vagrant-qemu"),
            Provider::Utm => Some("vagrant_utm"),
            Provider::VmwareEsxi => Some("vagrant-vmware-esxi"),
            Provider::Proxmox => Some("vagrant-proxmox"),
        }
    }

    /// The local tool that proves the hypervisor itself is installed, if there is one.
    fn probe(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            Provider::Virtualbox => Some(("VBoxManage", &["--version"])),
            Provider::VmwareDesktop => Some(("vmrun", &[])),
            Provider::Parallels => Some(("prlctl", &["--version"])),
            Provider::Libvirt => Some(("virsh", &["--version"])),
            Provider::Qemu => Some((qemu_binary(), &["--version"])),
            _ => None,
        }
    }

    /// Whether the hypervisor itself is installed here, `None` when there is nothing local to
    /// check (remote providers). UTM is an app bundle; Hyper-V is a Windows feature.
    async fn hypervisor_installed(self, runner: &dyn ProcessRunner) -> Option<bool> {
        match self {
            Provider::Utm => Some(Path::new(UTM_APP).exists()),
            Provider::Hyperv => Some(hyperv_enabled()),
            _ => {
                let (program, args) = self.probe()?;
                Some(tool_present(runner, program, args).await)
            }
        }
    }
}

/// Where UTM is installed on macOS.
const UTM_APP: &str = "/Applications/UTM.app";

/// Whether the Hyper-V feature is on: its Virtual Machine Management service is installed with it.
fn hyperv_enabled() -> bool {
    let root = std::env::var_os("SystemRoot").map_or_else(|| std::path::PathBuf::from("C:\\Windows"), std::path::PathBuf::from);
    root.join("System32").join("vmms.exe").is_file()
}

/// Why libvirt can't run VMs here although `virsh` is installed: vagrant-libvirt boots KVM guests,
/// so it needs `/dev/kvm`, and the player must be allowed to open it. `None` when it is fine.
#[must_use]
pub fn kvm_problem() -> Option<String> {
    let dev = Path::new("/dev/kvm");
    if !dev.exists() {
        return Some("KVM isn't available on this machine (no /dev/kvm): turn on virtualization in the firmware, or use another hypervisor".to_owned());
    }
    if std::fs::OpenOptions::new().read(true).write(true).open(dev).is_err() {
        return Some("you can't use /dev/kvm yet: add yourself to the kvm and libvirt groups, then sign out and back in".to_owned());
    }
    None
}

/// The QEMU binary for this machine's CPU.
fn qemu_binary() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "qemu-system-aarch64"
    } else {
        "qemu-system-x86_64"
    }
}

/// One provider's readiness, as the setup screen reads it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProviderStatus {
    /// The provider.
    pub provider: Provider,
    /// Whether it runs VMs on another host.
    pub remote: bool,
    /// Whether it is ready to use here.
    pub available: bool,
    /// Whether the hypervisor itself is installed, `None` when there is nothing to probe.
    pub hypervisor: Option<bool>,
    /// The Vagrant plugin it needs, if it is not built in.
    pub plugin: Option<String>,
    /// Whether that plugin is present (true when the provider is built in).
    pub plugin_installed: bool,
    /// Why it is not available, `None` when it is.
    pub reason: Option<String>,
}

/// The Vagrant plugin names in `vagrant plugin list` lines like `vagrant-libvirt (0.12.2, global)`.
#[must_use]
pub fn parse_plugins(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|name| name.starts_with("vagrant-") || name.starts_with("vagrant_"))
        .map(str::to_owned)
        .collect()
}

/// Whether `program` is installed: it is, as long as it could be started (a tool that prints usage
/// and exits non-zero, like `vmrun`, still exists).
async fn tool_present(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> bool {
    exec::read(runner, program, args, None).await.is_ok()
}

/// The Vagrant plugins installed, empty when `vagrant plugin list` can't run.
async fn installed_plugins(runner: &dyn ProcessRunner) -> Vec<String> {
    match exec::read(runner, "vagrant", &["plugin", "list"], None).await {
        Ok(out) => parse_plugins(&out),
        Err(_) => Vec::new(),
    }
}

/// How long the VirtualBox liveness probe may take before VirtualBox counts as wedged.
const VBOX_PROBE: Duration = Duration::from_secs(8);

/// The readiness of every provider usable on this machine: its hypervisor, its plugin, and why it
/// is not available. `vagrant_installed` is global, so it is a plain argument.
pub async fn detect(runner: &dyn ProcessRunner, vagrant_installed: bool) -> Vec<ProviderStatus> {
    let plugins = if vagrant_installed { installed_plugins(runner).await } else { Vec::new() };
    let mut statuses = Vec::new();
    for provider in Provider::ALL {
        if !provider.applicable() {
            continue;
        }
        let hypervisor = provider.hypervisor_installed(runner).await;
        let plugin = provider.plugin().map(str::to_owned);
        let plugin_installed = match &plugin {
            None => true,
            Some(needed) => vagrant_installed && plugins.iter().any(|i| i == needed),
        };
        let unresponsive = provider == Provider::Virtualbox && hypervisor == Some(true) && vbox_wedged(runner).await;
        let detection = Detection { hypervisor, unresponsive, plugin: plugin.as_deref(), plugin_installed };
        let reason = unavailable_reason(provider, vagrant_installed, detection);
        statuses.push(ProviderStatus {
            provider,
            remote: provider.is_remote(),
            available: reason.is_none(),
            hypervisor,
            plugin,
            plugin_installed,
            reason,
        });
    }
    statuses
}

/// Whether VirtualBox's background service is wedged: `VBoxManage --version` never talks to it, so
/// ask it something that does and see whether that times out.
async fn vbox_wedged(runner: &dyn ProcessRunner) -> bool {
    let probe = exec::read(runner, "VBoxManage", &["list", "runningvms"], None);
    matches!(exec::within("VBoxManage", VBOX_PROBE, probe).await, Err(crate::Error::Timeout { .. }))
}

/// What `detect` learned about one provider, grouped so `unavailable_reason` reads one value
/// rather than a handful of flags that always travel together.
#[derive(Clone, Copy)]
struct Detection<'a> {
    /// Whether the hypervisor is installed, `None` when there is nothing to probe.
    hypervisor: Option<bool>,
    /// Whether the hypervisor's service is wedged (its liveness probe timed out).
    unresponsive: bool,
    /// The Vagrant plugin it needs, if it is not built in.
    plugin: Option<&'a str>,
    /// Whether that plugin is installed.
    plugin_installed: bool,
}

/// Why a provider can't be used here, `None` when it can: the first layer that is missing.
fn unavailable_reason(provider: Provider, vagrant_installed: bool, detection: Detection<'_>) -> Option<String> {
    let Detection { hypervisor, unresponsive, plugin, plugin_installed } = detection;
    if !vagrant_installed {
        Some("Vagrant is not installed".to_owned())
    } else if unresponsive {
        Some("VirtualBox isn't responding (its background service is stuck). Quit VirtualBox and any VM windows, or restart the computer, then re-check.".to_owned())
    } else if hypervisor == Some(false) && provider == Provider::Hyperv {
        Some("Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it".to_owned())
    } else if hypervisor == Some(false) {
        Some(provider.probe().map_or_else(|| "not installed".to_owned(), |(program, _)| format!("`{program}` was not found")))
    } else if let Some(why) = (provider == Provider::Libvirt).then(kvm_problem).flatten() {
        Some(why)
    } else if !plugin_installed {
        plugin.map(|p| format!("Vagrant plugin `{p}` is not installed"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    #[test]
    fn plugin_lines_are_parsed_to_names() {
        let out = "vagrant-libvirt (0.12.2, global)\nvagrant_utm (1.0.0)\nnot-a-plugin (1)\n";
        assert_eq!(parse_plugins(out), ["vagrant-libvirt", "vagrant_utm"]);
    }

    #[test]
    fn remote_and_cloud_are_classified() {
        assert!(Provider::Proxmox.is_remote() && !Provider::Proxmox.is_cloud());
        assert!(Provider::Aws.is_remote() && Provider::Aws.is_cloud());
        assert!(!Provider::Qemu.is_remote());
    }

    #[tokio::test]
    async fn detect_reports_a_ready_qemu_and_an_unavailable_one_by_its_plugin() {
        // QEMU installed, its plugin present: available. A missing plugin makes it unavailable.
        let with = FakeProcesses::rules(&[("qemu-system", Reply::ok("QEMU 10.0")), ("vagrant plugin list", Reply::ok("vagrant-qemu (0.6.3, global)"))]);
        let qemu = detect(&with, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert!(qemu.hypervisor == Some(true) && qemu.plugin_installed && qemu.available, "{qemu:?}");

        let without = FakeProcesses::rules(&[("qemu-system", Reply::ok("QEMU 10.0")), ("vagrant plugin list", Reply::ok(""))]);
        let no_plugin = detect(&without, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert!(!no_plugin.available && no_plugin.reason.is_some(), "{no_plugin:?}");
    }
}
