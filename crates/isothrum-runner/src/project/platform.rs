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

/// A Vagrant provider: the hypervisor `vagrant up --provider` takes. Exhaustive: whoever detects
/// or offers providers (`isothrum-machine`) fails to compile until a new one has its row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
// Its TypeScript type (the `ts-rs` feature) is `VagrantProvider`.
#[cfg_attr(feature = "ts-rs", derive(ts_rs::TS), ts(rename = "VagrantProvider"))]
#[serde(rename_all = "snake_case")]
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
    /// Every provider, in the order they are offered.
    pub const ALL: [Provider; 8] = [
        Provider::Virtualbox,
        Provider::VmwareDesktop,
        Provider::Hyperv,
        Provider::Parallels,
        Provider::Libvirt,
        Provider::Qemu,
        Provider::Utm,
        Provider::VmwareEsxi,
    ];

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

    /// The plugin Vagrant runs it through; `None` for the providers Vagrant has built in.
    #[must_use]
    pub const fn plugin(self) -> Option<&'static str> {
        match self {
            Provider::Virtualbox | Provider::Hyperv => None,
            Provider::VmwareDesktop => Some("vagrant-vmware-desktop"),
            Provider::Parallels => Some("vagrant-parallels"),
            Provider::Libvirt => Some("vagrant-libvirt"),
            Provider::Qemu => Some("vagrant-qemu"),
            Provider::Utm => Some("vagrant_utm"),
            Provider::VmwareEsxi => Some("vagrant-vmware-esxi"),
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

/// A port's transport protocol.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    /// TCP.
    #[default]
    Tcp,
    /// UDP.
    Udp,
}

impl Protocol {
    /// The protocol as the tools write it (`tcp`, `udp`), in any case; `None` for another word.
    #[must_use]
    pub fn parse(word: &str) -> Option<Protocol> {
        if word.eq_ignore_ascii_case("tcp") {
            Some(Protocol::Tcp)
        } else if word.eq_ignore_ascii_case("udp") {
            Some(Protocol::Udp)
        } else {
            None
        }
    }

    /// The protocol as the tools write it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
        }
    }
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A tool's version: `major.minor.patch`, with an optional pre-release (`2.17.0`, `1.0.0-rc.1`).
/// Ordered as versions are (semver precedence): by the numbers, then a pre-release before the
/// release it leads to, two pre-releases by their dot-separated fields (numbers as numbers,
/// a number before text, fewer fields first when they tie).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    pre: Option<String>,
}

impl Version {
    /// `major.minor.patch`, released.
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: None,
        }
    }

    /// The version `text` is exactly (three numbers joined by dots, then optionally `-` and a
    /// pre-release of letters, digits, dots and dashes); `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Version> {
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.').map(|n| {
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            n.parse::<u64>().ok()
        });
        let (major, minor, patch) = (numbers.next()??, numbers.next()??, numbers.next()??);
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => None,
            Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-')) => Some(p.to_owned()),
            Some(_) => return None,
        };
        Some(Version { major, minor, patch, pre })
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Version) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => pre_release_cmp(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Version) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Two pre-releases, field by field: numbers as numbers, a number before text, text as ASCII,
/// and fewer fields first when the fields they have tie.
#[expect(clippy::many_single_char_names, reason = "the two sides of a comparison, field by field")]
fn pre_release_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let mut xs = a.split('.');
    let mut ys = b.split('.');
    loop {
        let order = match (xs.next(), ys.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(m), Ok(n)) => m.cmp(&n),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_sorts_by_its_numbers_then_a_pre_release_first() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(v("2.9.0") < v("2.14.0"));
        assert!(v("10.0.0") > v("9.0.0"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.9") < v("1.0.0-alpha.11"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        assert_eq!(v("2.17.0-desktop.1").to_string(), "2.17.0-desktop.1");
        for bad in ["1.0", "1.0.0-", "v1.0.0", "1..0", "1.0.0.0"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }
}
