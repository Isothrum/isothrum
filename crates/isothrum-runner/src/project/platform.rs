//! The host and hypervisor words Isothrum shares with its embedder: the computer's OS and CPU, a
//! Vagrant provider (the hypervisor `vagrant up --provider` takes) and a cloud. Isothrum's own
//! types, written as the tools and their users write them (`vmware_desktop`, `aws`).

use serde::{Deserialize, Serialize};

/// A host operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HostOs {
    /// Linux.
    Linux,
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

/// A CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Arch {
    /// x86-64.
    Amd64,
    /// 64-bit ARM.
    Arm64,
}

/// A Vagrant provider: the hypervisor `vagrant up --provider` takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Provider {
    /// VirtualBox.
    Virtualbox,
    /// VMware Workstation or Fusion.
    VmwareDesktop,
    /// Hyper-V.
    Hyperv,
    /// Parallels Desktop.
    Parallels,
    /// libvirt (KVM).
    Libvirt,
    /// QEMU.
    Qemu,
    /// UTM.
    Utm,
    /// An ESXi server.
    VmwareEsxi,
}

impl Provider {
    /// The provider as `vagrant up --provider` takes it (`vmware_desktop`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Provider::Virtualbox => "virtualbox",
            Provider::VmwareDesktop => "vmware_desktop",
            Provider::Hyperv => "hyperv",
            Provider::Parallels => "parallels",
            Provider::Libvirt => "libvirt",
            Provider::Qemu => "qemu",
            Provider::Utm => "utm",
            Provider::VmwareEsxi => "vmware_esxi",
        }
    }

    /// Whether its VMs run on another computer (a server), not this one.
    #[must_use]
    pub const fn is_remote(self) -> bool {
        matches!(self, Provider::VmwareEsxi)
    }
}

/// A public cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Cloud {
    /// Amazon Web Services.
    Aws,
    /// Microsoft Azure.
    Azure,
    /// Google Cloud.
    Gcp,
    /// DigitalOcean.
    Digitalocean,
    /// Akamai Linode.
    Linode,
    /// Oracle Cloud Infrastructure.
    Oci,
}

/// The run manifest's words for the same things, read into Isothrum's while the runner still
/// reads manifests. Removed with the manifest.
mod from_manifest {
    use super::{Arch, Cloud, HostOs, Provider};

    impl From<isoloom_manifest::HostOs> for HostOs {
        fn from(os: isoloom_manifest::HostOs) -> HostOs {
            match os {
                isoloom_manifest::HostOs::Linux => HostOs::Linux,
                isoloom_manifest::HostOs::Macos => HostOs::Macos,
                isoloom_manifest::HostOs::Windows => HostOs::Windows,
            }
        }
    }

    impl From<isoloom_manifest::Arch> for Arch {
        fn from(arch: isoloom_manifest::Arch) -> Arch {
            match arch {
                isoloom_manifest::Arch::Amd64 => Arch::Amd64,
                isoloom_manifest::Arch::Arm64 => Arch::Arm64,
            }
        }
    }

    impl From<isoloom_manifest::Provider> for Provider {
        fn from(p: isoloom_manifest::Provider) -> Provider {
            match p {
                isoloom_manifest::Provider::Virtualbox => Provider::Virtualbox,
                isoloom_manifest::Provider::VmwareDesktop => Provider::VmwareDesktop,
                isoloom_manifest::Provider::Hyperv => Provider::Hyperv,
                isoloom_manifest::Provider::Parallels => Provider::Parallels,
                isoloom_manifest::Provider::Libvirt => Provider::Libvirt,
                isoloom_manifest::Provider::Qemu => Provider::Qemu,
                isoloom_manifest::Provider::Utm => Provider::Utm,
                isoloom_manifest::Provider::VmwareEsxi => Provider::VmwareEsxi,
            }
        }
    }

    impl From<isoloom_manifest::Cloud> for Cloud {
        fn from(c: isoloom_manifest::Cloud) -> Cloud {
            match c {
                isoloom_manifest::Cloud::Aws => Cloud::Aws,
                isoloom_manifest::Cloud::Azure => Cloud::Azure,
                isoloom_manifest::Cloud::Gcp => Cloud::Gcp,
                isoloom_manifest::Cloud::Digitalocean => Cloud::Digitalocean,
                isoloom_manifest::Cloud::Linode => Cloud::Linode,
                isoloom_manifest::Cloud::Oci => Cloud::Oci,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every word reads as the manifest's did, so a record or an embedder's setting written with
    /// one reads with the other.
    #[test]
    fn the_words_are_the_manifests() {
        for p in [
            isoloom_manifest::Provider::Virtualbox,
            isoloom_manifest::Provider::VmwareDesktop,
            isoloom_manifest::Provider::Hyperv,
            isoloom_manifest::Provider::Parallels,
            isoloom_manifest::Provider::Libvirt,
            isoloom_manifest::Provider::Qemu,
            isoloom_manifest::Provider::Utm,
            isoloom_manifest::Provider::VmwareEsxi,
        ] {
            let ours = Provider::from(p);
            assert_eq!(serde_json::to_value(ours).ok(), serde_json::to_value(p).ok());
            assert_eq!(ours.id(), p.id());
        }
        for c in [
            isoloom_manifest::Cloud::Aws,
            isoloom_manifest::Cloud::Oci,
            isoloom_manifest::Cloud::Digitalocean,
        ] {
            assert_eq!(serde_json::to_value(Cloud::from(c)).ok(), serde_json::to_value(c).ok());
        }
    }
}
