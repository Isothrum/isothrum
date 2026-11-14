//! Vagrant's providers as this driver runs them: the host systems each runs on, and the plugin
//! Vagrant needs for it.

use crate::project::platform::{HostOs, Provider};

/// Every provider the driver runs.
pub(super) const ALL: [Provider; 8] = [
    Provider::Virtualbox,
    Provider::VmwareDesktop,
    Provider::Hyperv,
    Provider::Parallels,
    Provider::Libvirt,
    Provider::Qemu,
    Provider::Utm,
    Provider::VmwareEsxi,
];

/// The host systems `provider` runs on: Hyper-V only on Windows, Parallels and UTM only on macOS,
/// libvirt only on Linux, QEMU on Linux and macOS; the others everywhere (ESXi is a server).
pub(super) fn hosts(provider: Provider) -> &'static [HostOs] {
    const EVERY: &[HostOs] = &[HostOs::Linux, HostOs::Macos, HostOs::Windows];
    match provider {
        Provider::Hyperv => &[HostOs::Windows],
        Provider::Parallels | Provider::Utm => &[HostOs::Macos],
        Provider::Libvirt => &[HostOs::Linux],
        Provider::Qemu => &[HostOs::Linux, HostOs::Macos],
        _ => EVERY,
    }
}

/// The plugin Vagrant runs `provider` through; `None` for the providers Vagrant has built in.
pub(super) fn plugin(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::VmwareDesktop => Some("vagrant-vmware-desktop"),
        Provider::Parallels => Some("vagrant-parallels"),
        Provider::Libvirt => Some("vagrant-libvirt"),
        Provider::Qemu => Some("vagrant-qemu"),
        Provider::Utm => Some("vagrant_utm"),
        Provider::VmwareEsxi => Some("vagrant-vmware-esxi"),
        _ => None,
    }
}

/// This computer's system.
pub(super) fn this_host() -> HostOs {
    match std::env::consts::OS {
        "windows" => HostOs::Windows,
        "macos" => HostOs::Macos,
        _ => HostOs::Linux,
    }
}

/// The providers that run on `host`, as `vagrant up --provider` takes them, quoted and listed.
pub(super) fn listed_for(host: HostOs) -> String {
    ALL.iter()
        .filter(|p| hosts(**p).contains(&host))
        .map(|p| format!("`{}`", p.id()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_host_runs_only_its_own_hypervisors() {
        assert_eq!(listed_for(HostOs::Windows), "`virtualbox`, `vmware_desktop`, `hyperv`, `vmware_esxi`");
        assert_eq!(
            listed_for(HostOs::Macos),
            "`virtualbox`, `vmware_desktop`, `parallels`, `qemu`, `utm`, `vmware_esxi`"
        );
        assert_eq!(listed_for(HostOs::Linux), "`virtualbox`, `vmware_desktop`, `libvirt`, `qemu`, `vmware_esxi`");
        assert_eq!(plugin(Provider::Virtualbox), None);
        assert_eq!(plugin(Provider::Hyperv), None);
        assert_eq!(plugin(Provider::Libvirt), Some("vagrant-libvirt"));
    }
}
