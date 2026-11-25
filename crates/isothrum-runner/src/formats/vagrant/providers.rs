//! Vagrant's providers as this driver runs them: the host systems each runs on (each one's
//! plugin is [`Provider::plugin`]).

use crate::project::platform::{HostOs, Provider};

/// Every provider the driver runs.
pub(super) const ALL: [Provider; 8] = Provider::ALL;

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
        assert_eq!(Provider::Virtualbox.plugin(), None);
        assert_eq!(Provider::Hyperv.plugin(), None);
        assert_eq!(Provider::Libvirt.plugin(), Some("vagrant-libvirt"));
    }
}
