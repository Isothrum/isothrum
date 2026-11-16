//! Why a provider can't run VMs here, from what detection found.

use super::{Account, Findings, Need, Part, Provider, ProviderRow, Unavailable, UtilityState};

/// Why `provider` can't run VMs here (`None`: it can), from what is still to install for it
/// (`needs`, which its status carries too, [`status_from`]) and what detection found: KVM holding the CPU's
/// virtualization first (nothing a setup installs mends it), then Vagrant, then the hypervisor
/// and the account, the machine's virtualization, the versions, the plugin and the VMware
/// utility, and VirtualBox's kernel driver last. It is `None` only when nothing is needed, so
/// readiness and a setup's steps can't disagree.
pub(super) fn unavailable_reason(provider: Provider, needs: &[Need], f: &Findings) -> Option<Unavailable> {
    f.held
        .clone()
        .or_else(|| needs.contains(&Need::Vagrant).then_some(Unavailable::VagrantMissing))
        .or_else(|| hypervisor_reason(provider, f))
        .or_else(|| f.platform.clone())
        .or_else(|| version_reason(provider, f))
        .or_else(|| plugin_reason(needs, f))
        .or_else(|| f.driver.clone())
}

/// Why the hypervisor itself, or this account, keeps `provider` from running VMs.
pub(super) fn hypervisor_reason(provider: Provider, f: &Findings) -> Option<Unavailable> {
    if f.unresponsive {
        Some(Unavailable::VirtualboxStuck)
    } else if f.hypervisor == Some(false) && provider == Provider::Hyperv {
        Some(Unavailable::HypervOff)
    } else if f.account == Some(Account::HypervAdmin) {
        Some(Unavailable::HypervNeedsAdmin)
    } else if f.part_missing == Some(Part::Mkisofs) {
        Some(Unavailable::MkisofsMissing)
    } else if f.part_missing == Some(Part::Virtiofsd) {
        Some(Unavailable::VirtiofsdMissing {
            hypervisor: hypervisor_name(provider).to_owned(),
        })
    } else if f.hypervisor == Some(false) {
        Some(Unavailable::NotInstalled {
            program: f.program.map(str::to_owned),
        })
    } else if let Some(Account::NewLogin(group)) = &f.account {
        Some(Unavailable::NewLogin { group: group.clone() })
    } else {
        None
    }
}

/// The name `provider`'s hypervisor goes by in a reason: the provider's own, written as its
/// maker writes it.
fn hypervisor_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Virtualbox => "VirtualBox",
        Provider::VmwareDesktop => "VMware",
        Provider::Hyperv => "Hyper-V",
        Provider::Parallels => "Parallels",
        Provider::Libvirt => "libvirt",
        Provider::Qemu => "QEMU",
        Provider::Utm => "UTM",
        Provider::VmwareEsxi => "VMware ESXi",
    }
}

/// Which of `provider`'s hypervisor (said first: updating the plugin wouldn't help) and plugin
/// is older than it is held to.
pub(super) fn version_reason(provider: Provider, f: &Findings) -> Option<Unavailable> {
    if let Some((name, version, min)) = hypervisor_too_old(provider, f.hypervisor_version.as_deref()) {
        return Some(Unavailable::HypervisorTooOld {
            name: name.to_owned(),
            version: version.to_owned(),
            min: min.to_owned(),
        });
    }
    let (version, min) = plugin_too_old(provider, f.plugin_version.as_deref())?;
    Some(Unavailable::PluginTooOld {
        plugin: f.plugin.unwrap_or_default().to_owned(),
        version: version.to_owned(),
        min: min.to_owned(),
    })
}

/// Why the provider's plugin layer isn't ready: its plugins couldn't be listed, else the first
/// of its `needs`.
pub(super) fn plugin_reason(needs: &[Need], f: &Findings) -> Option<Unavailable> {
    let plugin = || f.plugin.unwrap_or_default().to_owned();
    if let (Some(_), Some(why)) = (f.plugin, &f.plugins_unknown) {
        return Some(Unavailable::PluginsUnknown {
            plugin: plugin(),
            reason: why.clone(),
        });
    }
    needs.first().map(|need| match need {
        Need::Vagrant => Unavailable::VagrantMissing,
        Need::Plugin => Unavailable::PluginMissing { plugin: plugin() },
        Need::VmwareUtility if f.utility == Some(UtilityState::Stopped) => Unavailable::VmwareUtilityStopped,
        Need::VmwareUtility => Unavailable::VmwareUtilityMissing,
    })
}

/// The version `installed` of `provider`'s plugin when it is older than the one that works
/// ([`ProviderRow::plugin_minimum`]), with that minimum; `None` when it is recent enough, unknown,
/// or the plugin has no minimum.
pub(super) fn plugin_too_old(provider: Provider, installed: Option<&str>) -> Option<(&str, &'static str)> {
    let (version, min) = installed.zip(ProviderRow::of(provider).plugin_minimum)?;
    older(version, min).then_some((version, min))
}

/// The version `installed` of `provider`'s hypervisor when it is older than the one its plugin
/// drives ([`ProviderRow::hypervisor_minimum`]), with the hypervisor's name and that minimum.
pub(super) fn hypervisor_too_old(provider: Provider, installed: Option<&str>) -> Option<(&'static str, &str, &'static str)> {
    let (version, (name, min)) = installed.zip(ProviderRow::of(provider).hypervisor_minimum)?;
    older(version, min).then_some((name, version, min))
}

/// Whether the version `installed` says is before `min`, in the runner's order (a pre-release before
/// its release); not when either says no version.
pub(super) fn older(installed: &str, min: &str) -> bool {
    isothrum_runner::version_found(installed)
        .zip(isothrum_runner::version_found(min))
        .is_some_and(|(found, min)| found < min)
}
