//! Why a provider can't run VMs here, as a code a UI says in its user's language (with the values
//! its sentence needs), and its English sentence for logs and the command line.

use serde::{Deserialize, Serialize};

/// Why a provider can't run VMs here: the first layer missing (Vagrant, the hypervisor, the
/// account's rights, the machine's virtualization, its plugin, the VMware utility), as
/// [`detect`](super::detect) found it. Its serde form is `{"code": "pluginMissing", "args":
/// {"plugin": "vagrant-libvirt"}}`: a UI translates the code and fills in the args.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "code", content = "args", rename_all = "camelCase", rename_all_fields = "camelCase")]
#[non_exhaustive]
pub enum Unavailable {
    /// Vagrant isn't installed.
    VagrantMissing,
    /// VirtualBox's background service is stuck: a call to it timed out.
    VirtualboxStuck,
    /// Hyper-V isn't turned on, or this edition of Windows doesn't have it.
    HypervOff,
    /// This account may not manage Hyper-V, which Vagrant needs (an administrator or a member of
    /// Hyper-V Administrators may).
    HypervNeedsAdmin,
    /// Hyper-V is turned on, but Windows' hypervisor isn't running: virtualization is off in the
    /// firmware, or `hypervisorlaunchtype` is off (often set so VirtualBox runs faster).
    HypervisorStopped,
    /// QEMU is installed, but not `mkisofs` (Linux), which Vagrant builds the cloud-init seed that
    /// gives a QEMU VM its network with: QEMU's install brings it (`genisoimage`).
    MkisofsMissing,
    /// libvirt or QEMU is installed, but finds no virtiofsd (Linux), which it shares a folder of
    /// this computer with a VM through: virtiofsd is its own package (`virtiofsd`).
    VirtiofsdMissing {
        /// The hypervisor's name (`libvirt`, `QEMU`).
        hypervisor: String,
    },
    /// The hypervisor isn't installed.
    NotInstalled {
        /// The program looked for, which wasn't found; `None` for a hypervisor found otherwise
        /// (an app bundle).
        program: Option<String>,
    },
    /// The user joined `group` since logging in (libvirt's, or the group `/dev/kvm` is opened
    /// through, for QEMU): it applies from the next login.
    NewLogin {
        /// The group joined.
        group: String,
    },
    /// This machine has no `/dev/kvm`: virtualization is off in the firmware, or there is none.
    NoKvm,
    /// `/dev/kvm` exists, but this account may not open it: it isn't in `group`, the one
    /// `/dev/kvm` belongs to (`kvm` on Debian, Ubuntu, Kali and openSUSE, whose `/dev/kvm` is
    /// `0660`; Fedora's and Arch's is `0666`, open to everyone).
    KvmDenied {
        /// The group `/dev/kvm` belongs to.
        group: String,
    },
    /// `/dev/kvm` exists and this account may open it, but opening it failed otherwise (the
    /// device busy, no KVM behind it).
    KvmUnusable {
        /// What the OS said.
        error: String,
    },
    /// The hypervisor is older than its Vagrant plugin drives.
    HypervisorTooOld {
        /// The hypervisor's name.
        name: String,
        /// The version installed.
        version: String,
        /// The oldest version the plugin drives.
        min: String,
    },
    /// The Vagrant plugin is older than the one that works: installing it again updates it.
    PluginTooOld {
        /// The plugin's name.
        plugin: String,
        /// The version installed.
        version: String,
        /// The oldest version that works.
        min: String,
    },
    /// The provider's Vagrant plugin isn't installed.
    PluginMissing {
        /// The plugin's name.
        plugin: String,
    },
    /// Whether the provider's Vagrant plugin is installed can't be told: `vagrant plugin list`
    /// failed (not read as "not installed": the user would be told to install a plugin that may
    /// be there).
    PluginsUnknown {
        /// The plugin's name.
        plugin: String,
        /// What `vagrant plugin list` said.
        reason: String,
    },
    /// The Vagrant VMware Utility is installed, but its service doesn't answer.
    VmwareUtilityStopped,
    /// The Vagrant VMware Utility, HashiCorp's service the VMware plugin drives VMware through,
    /// isn't installed.
    VmwareUtilityMissing,
    /// VirtualBox is installed, but its kernel driver (`vboxdrv`) isn't loaded on the running
    /// kernel (Linux), so no VM starts: never built (the kernel's headers missing), or refused
    /// (Secure Boot).
    VirtualboxDriver {
        /// The running kernel (`uname -r`).
        kernel: String,
    },
    /// VirtualBox's kernel driver waits for a restart (Linux): the running kernel has no headers,
    /// and a newer kernel installed with its headers has, which the driver is built for when the
    /// computer starts it.
    KernelRestart {
        /// The newer kernel installed (`uname -r` once it runs).
        kernel: String,
    },
    /// KVM holds the CPU's virtualization extensions (Linux): its module (`kvm_intel`, `kvm_amd`)
    /// turns them on as it loads (`kvm.enable_virt_at_load`, the default since Linux 6.12), so no
    /// VirtualBox VM starts ("VT-x is being used by another hypervisor"), installed or not.
    KvmHoldsVirtualization,
}

impl Unavailable {
    /// The English sentence, for logs and the command line; a UI says the code in its own words.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Unavailable::VagrantMissing => "Vagrant is not installed.".to_owned(),
            Unavailable::VirtualboxStuck => {
                "VirtualBox isn't responding (its background service is stuck): quit VirtualBox and any VM windows, or restart the computer.".to_owned()
            }
            Unavailable::HypervOff => "Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it.".to_owned(),
            Unavailable::HypervNeedsAdmin => "Vagrant drives Hyper-V only for an administrator or a member of the Hyper-V Administrators group: \
                add this account to that group and sign out of Windows and back in, or run elevated."
                .to_owned(),
            Unavailable::HypervisorStopped => "Hyper-V is turned on but Windows' hypervisor isn't running: turn virtualization on in the firmware, \
                or run `bcdedit /set hypervisorlaunchtype auto` as an administrator, then restart Windows."
                .to_owned(),
            Unavailable::MkisofsMissing => "QEMU VMs need mkisofs, which isn't installed: Vagrant builds with it the disk that gives each VM its network. \
                Install QEMU again (that adds it), or install genisoimage."
                .to_owned(),
            Unavailable::VirtiofsdMissing { hypervisor } => {
                format!("{hypervisor} needs virtiofsd to share a folder of this computer with a VM, and finds none: install the virtiofsd package.")
            }
            Unavailable::NotInstalled { program: Some(program) } => format!("`{program}` was not found."),
            Unavailable::NotInstalled { program: None } => "Not installed.".to_owned(),
            Unavailable::NewLogin { group } => {
                format!("You were added to the {group} group since you logged in: log out and back in (or restart) for it to apply.")
            }
            Unavailable::NoKvm => {
                "KVM isn't available on this machine (no /dev/kvm): turn on virtualization in the firmware, or use another hypervisor.".to_owned()
            }
            Unavailable::KvmDenied { group } => format!("You can't use /dev/kvm yet: add yourself to the {group} group, then sign out and back in."),
            Unavailable::KvmUnusable { error } => format!("/dev/kvm can't be opened: {error}."),
            Unavailable::HypervisorTooOld { name, version, min } => format!("{name} {version} is too old: {min} or later is needed. Update {name}."),
            Unavailable::PluginTooOld { plugin, version, min } => {
                format!("Vagrant plugin `{plugin}` {version} is too old: {min} or later is needed. Install it again to update it.")
            }
            Unavailable::PluginMissing { plugin } => format!("Vagrant plugin `{plugin}` is not installed."),
            Unavailable::PluginsUnknown { plugin, reason } => {
                format!("Whether Vagrant plugin `{plugin}` is installed can't be told: `vagrant plugin list` failed ({reason}).")
            }
            Unavailable::VmwareUtilityStopped => {
                "The Vagrant VMware Utility is installed, but its service isn't running: install it again (that restarts it), or restart the computer."
                    .to_owned()
            }
            Unavailable::VmwareUtilityMissing => {
                "The Vagrant VMware Utility is not installed: HashiCorp's service the VMware plugin drives VMware through.".to_owned()
            }
            Unavailable::VirtualboxDriver { kernel } => format!(
                "VirtualBox's kernel driver (vboxdrv) isn't loaded on this kernel ({kernel}), so VirtualBox can't start VMs. \
                Install the kernel's headers (linux-headers-{kernel} on Debian and Ubuntu), then build the driver \
                (`sudo /sbin/vboxconfig` for Oracle's VirtualBox, `sudo dpkg-reconfigure virtualbox-dkms` for the distribution's). \
                With Secure Boot on, the driver must be signed before it loads."
            ),
            Unavailable::KernelRestart { kernel } => {
                format!("VirtualBox's kernel driver is built for the newer kernel installed with it ({kernel}): restart the computer to start that kernel.")
            }
            Unavailable::KvmHoldsVirtualization => {
                "KVM holds this computer's hardware virtualization (VT-x or AMD-V): Linux turns it on as soon as KVM's module loads \
                (the default since Linux 6.12), so VirtualBox can't start VMs (\"VT-x is being used by another hypervisor\"). \
                Use libvirt or QEMU, which run on KVM; for VirtualBox, unload KVM (`sudo modprobe -r kvm_intel kvm_amd`) \
                or start Linux with `kvm.enable_virt_at_load=0`."
                    .to_owned()
            }
        }
    }
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code, with sample values.
    fn samples() -> Vec<Unavailable> {
        let s = String::from;
        vec![
            Unavailable::VagrantMissing,
            Unavailable::VirtualboxStuck,
            Unavailable::HypervOff,
            Unavailable::HypervNeedsAdmin,
            Unavailable::HypervisorStopped,
            Unavailable::MkisofsMissing,
            Unavailable::VirtiofsdMissing { hypervisor: s("QEMU") },
            Unavailable::NotInstalled { program: Some(s("virsh")) },
            Unavailable::NotInstalled { program: None },
            Unavailable::NewLogin { group: s("libvirt") },
            Unavailable::NoKvm,
            Unavailable::KvmDenied { group: s("kvm") },
            Unavailable::KvmUnusable {
                error: s("Device or resource busy"),
            },
            Unavailable::HypervisorTooOld {
                name: s("UTM"),
                version: s("4.4.5"),
                min: s("4.6.5"),
            },
            Unavailable::PluginTooOld {
                plugin: s("vagrant_utm"),
                version: s("0.1.0"),
                min: s("0.2.0"),
            },
            Unavailable::PluginMissing { plugin: s("vagrant-libvirt") },
            Unavailable::PluginsUnknown {
                plugin: s("vagrant-libvirt"),
                reason: s("exited with code 1"),
            },
            Unavailable::VmwareUtilityStopped,
            Unavailable::VmwareUtilityMissing,
            Unavailable::VirtualboxDriver {
                kernel: s("6.12.86+deb13-amd64"),
            },
            Unavailable::KernelRestart {
                kernel: s("6.12.116+deb13-amd64"),
            },
            Unavailable::KvmHoldsVirtualization,
        ]
    }

    /// The wire form a UI translates: the code in camelCase, the args by name, a code without
    /// args without them; and it reads back as it was.
    #[test]
    fn a_reason_is_a_code_with_its_args() {
        assert_eq!(
            serde_json::to_value(Unavailable::PluginMissing {
                plugin: "vagrant-libvirt".into()
            })
            .unwrap(),
            serde_json::json!({ "code": "pluginMissing", "args": { "plugin": "vagrant-libvirt" } })
        );
        assert_eq!(
            serde_json::to_value(Unavailable::NotInstalled { program: None }).unwrap(),
            serde_json::json!({ "code": "notInstalled", "args": { "program": null } })
        );
        assert_eq!(
            serde_json::to_value(Unavailable::HypervisorStopped).unwrap(),
            serde_json::json!({ "code": "hypervisorStopped" })
        );
        assert_eq!(
            serde_json::to_value(Unavailable::KvmHoldsVirtualization).unwrap(),
            serde_json::json!({ "code": "kvmHoldsVirtualization" })
        );
        for reason in samples() {
            let wire = serde_json::to_value(&reason).unwrap();
            assert_eq!(serde_json::from_value::<Unavailable>(wire.clone()).unwrap(), reason, "{wire}");
        }
    }

    /// Each sentence is one sentence that names every value it was given, and speaks to no
    /// particular program (no app name, no "re-check").
    #[test]
    fn every_reason_says_its_values_in_a_sentence_of_its_own() {
        for reason in samples() {
            let said = reason.message();
            assert!(said.ends_with('.'), "{said}");
            assert!(!said.contains("re-check") && !said.contains("Cyber CTF"), "{said}");
            assert_eq!(reason.to_string(), said);
            let wire = serde_json::to_value(&reason).unwrap();
            for value in wire.get("args").and_then(|a| a.as_object()).into_iter().flat_map(|a| a.values()) {
                if let Some(value) = value.as_str() {
                    assert!(said.contains(value), "{said} doesn't say {value}");
                }
            }
        }
    }
}
