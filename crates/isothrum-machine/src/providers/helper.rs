//! The programs a hypervisor's VMs need beside it on this computer ([`helpers`]): the one list
//! its detection reads it installed by, a setup's install of it brings
//! ([`Helper::package`]), and a run's needs ask for. A hypervisor installed without one reads
//! as not installed, so a setup installs it again, which brings the program.

use super::{Provider, virtiofsd};
use crate::context::Context;
use crate::host::{HostOs, LinuxPackages};

/// A program a hypervisor's VMs need beside it, which its install brings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Helper {
    /// `mkisofs`: vagrant-qemu gives a VM on a private network its settings in a cloud-init seed,
    /// an ISO Vagrant builds with `mkisofs` on Linux (`hdiutil` on macOS, which every Mac has).
    /// Without it, `vagrant up` fails at the first VM ("The executable 'mkisofs' Vagrant is
    /// trying to run was not found").
    Mkisofs,
    /// A virtiofsd the hypervisor finds ([`virtiofsd::virtiofsd`]), which libvirt and
    /// vagrant-qemu start to share a folder of this computer with a VM on Linux.
    Virtiofsd,
}

/// The programs `provider`'s VMs need beside its hypervisor on `os`, in the order they are
/// asked for: QEMU's on Linux `mkisofs` and a virtiofsd, libvirt's a virtiofsd.
#[must_use]
pub fn helpers(provider: Provider, os: HostOs) -> &'static [Helper] {
    match (provider, os) {
        (Provider::Qemu, HostOs::Linux) => &[Helper::Mkisofs, Helper::Virtiofsd],
        (Provider::Libvirt, HostOs::Linux) => &[Helper::Virtiofsd],
        _ => &[],
    }
}

impl Helper {
    /// The program's name.
    #[must_use]
    pub fn program(self) -> &'static str {
        match self {
            Helper::Mkisofs => "mkisofs",
            Helper::Virtiofsd => "virtiofsd",
        }
    }

    /// The package that brings it through `packages`: `genisoimage` has `/usr/bin/mkisofs` on
    /// Debian, Ubuntu and Fedora (Debian's `xorriso` has no `mkisofs`, only `xorrisofs`);
    /// `virtiofsd` is its own package from Debian 12, Ubuntu 23.10 and Fedora 36 on (QEMU's
    /// before), with the vhost-user descriptor libvirt finds it by.
    #[must_use]
    pub fn package(self, packages: LinuxPackages) -> &'static str {
        match (self, packages) {
            (Helper::Mkisofs, LinuxPackages::Apt | LinuxPackages::Dnf) => "genisoimage",
            (Helper::Virtiofsd, LinuxPackages::Apt | LinuxPackages::Dnf) => "virtiofsd",
        }
    }

    /// Whether `provider` finds it on the context's host: `mkisofs` starts, or the hypervisor
    /// finds a virtiofsd where it looks.
    pub async fn found(self, cx: &Context, provider: Provider) -> bool {
        match self {
            Helper::Mkisofs => cx.tool_starts("mkisofs", &["-version"]).await,
            Helper::Virtiofsd => virtiofsd::virtiofsd(provider, &cx.host.devices).is_some(),
        }
    }
}

/// The first of `provider`'s [`helpers`] the context's host hasn't; `None` when it has them all.
pub(crate) async fn missing(cx: &Context, provider: Provider) -> Option<Helper> {
    for &helper in helpers(provider, cx.host.os) {
        if !helper.found(cx, provider).await {
            return Some(helper);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No helper.
    const NONE: [Helper; 0] = [];

    #[test]
    fn qemu_on_linux_needs_mkisofs_and_virtiofsd_libvirt_virtiofsd_and_no_other() {
        assert_eq!(helpers(Provider::Qemu, HostOs::Linux), [Helper::Mkisofs, Helper::Virtiofsd]);
        assert_eq!(helpers(Provider::Libvirt, HostOs::Linux), [Helper::Virtiofsd]);
        // A Mac builds a VM's seed with hdiutil and shares no folder with a QEMU VM.
        assert_eq!(helpers(Provider::Qemu, HostOs::Macos), NONE);
        for p in [
            Provider::Virtualbox,
            Provider::VmwareDesktop,
            Provider::Hyperv,
            Provider::Utm,
            Provider::Parallels,
        ] {
            for os in crate::host::ALL_OSES {
                assert_eq!(helpers(p, os), NONE, "{p:?} on {os:?}");
            }
        }
    }

    #[test]
    fn each_helper_comes_in_a_package_of_every_linux_package_manager() {
        for helper in [Helper::Mkisofs, Helper::Virtiofsd] {
            for packages in [LinuxPackages::Apt, LinuxPackages::Dnf] {
                assert!(!helper.package(packages).is_empty(), "{helper:?} through {packages:?}");
            }
        }
        assert_eq!(Helper::Mkisofs.package(LinuxPackages::Apt), "genisoimage", "xorriso has no mkisofs");
    }
}
