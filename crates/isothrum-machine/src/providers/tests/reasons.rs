//! Why a provider is unavailable, from what detection found: the first layer missing (the
//! hypervisor, its turning on, a new login, Vagrant, the plugin, a tool its VMs need), and when a
//! provider is ready.

use super::*;

/// [`unavailable_reason`] with the needs [`status_from`] reads for `f`.
fn reason_of(provider: Provider, vagrant_installed: bool, f: &Findings) -> Option<Unavailable> {
    unavailable_reason(provider, &needs(vagrant_installed, f.plugin_installed, f.utility), f)
}

#[test]
fn the_first_missing_layer_is_the_reason() {
    let missing = Findings {
        hypervisor: Some(false),
        program: Some("VBoxManage"),
        ..installed()
    };
    let stuck = Findings {
        unresponsive: true,
        ..installed()
    };
    assert_eq!(reason_of(Provider::Virtualbox, false, &stuck), Some(Unavailable::VagrantMissing));
    assert_eq!(reason_of(Provider::Virtualbox, true, &stuck), Some(Unavailable::VirtualboxStuck));
    assert_eq!(
        reason_of(Provider::Virtualbox, true, &missing),
        Some(Unavailable::NotInstalled {
            program: Some("VBoxManage".into())
        })
    );
    let off = Findings {
        program: None,
        ..missing.clone()
    };
    assert_eq!(reason_of(Provider::Hyperv, true, &off), Some(Unavailable::HypervOff));
    assert_eq!(reason_of(Provider::Utm, true, &off), Some(Unavailable::NotInstalled { program: None }));
    let no_plugin = Findings {
        plugin: Some("vagrant-parallels"),
        plugin_installed: false,
        ..installed()
    };
    assert_eq!(
        reason_of(Provider::Parallels, true, &no_plugin),
        Some(Unavailable::PluginMissing {
            plugin: "vagrant-parallels".into()
        })
    );
    assert_eq!(reason_of(Provider::Virtualbox, true, &installed()), None);
    let esxi = Findings {
        hypervisor: None,
        plugin: Some("vagrant-vmware-esxi"),
        ..installed()
    };
    assert_eq!(reason_of(Provider::VmwareEsxi, true, &esxi), None);
}

#[test]
fn kvm_holding_the_cpus_virtualization_comes_before_anything_and_the_driver_after_everything() {
    let held = Findings {
        held: Some(Unavailable::KvmHoldsVirtualization),
        hypervisor: Some(false),
        ..installed()
    };
    assert_eq!(
        reason_of(Provider::Virtualbox, false, &held),
        Some(Unavailable::KvmHoldsVirtualization),
        "said before Vagrant"
    );
    let driver = Unavailable::VirtualboxDriver { kernel: "6.12.86".into() };
    let no_driver = Findings {
        driver: Some(driver.clone()),
        ..installed()
    };
    assert_eq!(reason_of(Provider::Virtualbox, false, &no_driver), Some(Unavailable::VagrantMissing));
    assert_eq!(reason_of(Provider::Virtualbox, true, &no_driver), Some(driver));
}

#[test]
fn hyperv_needs_admin_after_being_turned_on_and_its_hypervisor_running_after_that() {
    let admin = Findings {
        account: Some(Account::HypervAdmin),
        platform: Some(Unavailable::HypervisorStopped),
        ..installed()
    };
    assert_eq!(reason_of(Provider::Hyperv, true, &admin), Some(Unavailable::HypervNeedsAdmin));
    let stopped = Findings { account: None, ..admin };
    assert_eq!(reason_of(Provider::Hyperv, true, &stopped), Some(Unavailable::HypervisorStopped));
    assert_eq!(reason_of(Provider::Hyperv, true, &installed()), None);
}

#[test]
fn a_new_login_comes_before_kvm_and_vagrant_before_both() {
    let libvirt = Findings {
        plugin: Some("vagrant-libvirt"),
        account: Some(Account::NewLogin("libvirt".into())),
        platform: Some(Unavailable::KvmDenied { group: "kvm".into() }),
        ..installed()
    };
    assert_eq!(
        reason_of(Provider::Libvirt, true, &libvirt),
        Some(Unavailable::NewLogin { group: "libvirt".into() })
    );
    assert_eq!(reason_of(Provider::Libvirt, false, &libvirt), Some(Unavailable::VagrantMissing));
    let denied = Findings { account: None, ..libvirt };
    assert_eq!(
        reason_of(Provider::Libvirt, true, &denied),
        Some(Unavailable::KvmDenied { group: "kvm".into() })
    );
}

#[test]
fn qemu_without_mkisofs_says_its_vms_need_it_vagrant_first() {
    // Regression: QEMU read as ready without mkisofs, and every VM failed at its first boot
    // ("The executable 'mkisofs' Vagrant is trying to run was not found in the PATH variable").
    let qemu = Findings {
        hypervisor: Some(false),
        helper_missing: Some(Helper::Mkisofs),
        program: Some("qemu-system-x86_64"),
        plugin: Some("vagrant-qemu"),
        ..installed()
    };
    assert_eq!(reason_of(Provider::Qemu, true, &qemu), Some(Unavailable::MkisofsMissing));
    assert_eq!(reason_of(Provider::Qemu, false, &qemu), Some(Unavailable::VagrantMissing));
}

#[test]
fn libvirt_or_qemu_without_virtiofsd_says_its_shared_folders_need_it_vagrant_first() {
    let without = |provider: Provider| Findings {
        hypervisor: Some(false),
        helper_missing: Some(Helper::Virtiofsd),
        program: hypervisor_probe(provider, Arch::Amd64).map(|(program, _)| program),
        plugin: provider.plugin(),
        ..installed()
    };
    for (provider, name) in [(Provider::Libvirt, "libvirt"), (Provider::Qemu, "QEMU")] {
        let missing = Some(Unavailable::VirtiofsdMissing { hypervisor: name.into() });
        assert_eq!(reason_of(provider, true, &without(provider)), missing, "said instead of not installed");
        assert_eq!(reason_of(provider, false, &without(provider)), Some(Unavailable::VagrantMissing));
    }
}

#[test]
fn a_plugin_listing_that_failed_says_so_rather_than_plugin_missing() {
    let libvirt = |plugins_unknown: Option<&str>| Findings {
        plugin: Some("vagrant-libvirt"),
        plugin_installed: false,
        plugins_unknown: plugins_unknown.map(str::to_owned),
        ..installed()
    };
    assert_eq!(
        reason_of(Provider::Libvirt, true, &libvirt(Some("exited with code 1"))),
        Some(Unavailable::PluginsUnknown {
            plugin: "vagrant-libvirt".into(),
            reason: "exited with code 1".into()
        })
    );
    assert_eq!(
        reason_of(Provider::Libvirt, true, &libvirt(None)),
        Some(Unavailable::PluginMissing {
            plugin: "vagrant-libvirt".into()
        })
    );
    // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
    let virtualbox = Findings {
        plugins_unknown: Some("exited with code 1".into()),
        ..installed()
    };
    assert_eq!(reason_of(Provider::Virtualbox, true, &virtualbox), None);
}

#[test]
fn utm_and_its_plugin_are_held_to_a_version() {
    let utm = |hypervisor_version: Option<&str>, plugin_version: Option<&str>| Findings {
        hypervisor_version: hypervisor_version.map(str::to_owned),
        plugin: Some("vagrant_utm"),
        plugin_version: plugin_version.map(str::to_owned),
        plugin_installed: plugin_too_old(Provider::Utm, plugin_version).is_none(),
        ..installed()
    };
    assert_eq!(plugin_too_old(Provider::Utm, Some("0.1.6")), Some(("0.1.6", "0.2.0")));
    // Regression: a suffix was dropped, so a pre-release of the minimum passed for it; it comes
    // before its release, as the runner's tool probes order it.
    assert_eq!(plugin_too_old(Provider::Utm, Some("0.2.0-beta")), Some(("0.2.0-beta", "0.2.0")));
    assert_eq!(plugin_too_old(Provider::Utm, Some("unknown")), None, "no version to judge");
    assert_eq!(plugin_too_old(Provider::Utm, Some("0.10.0")), None, "10 > 2, not \"1\" < \"2\"");
    assert_eq!(hypervisor_too_old(Provider::Utm, Some("4.7")), None, "a missing part reads as 0");
    assert_eq!(hypervisor_too_old(Provider::Utm, Some("4.5.1")), Some(("UTM", "4.5.1", "4.6.5")));
    // The plugin the failure was seen with: installing it again updates it.
    let old_plugin = utm(Some("4.7.5"), Some("0.1.6"));
    assert!(!old_plugin.plugin_installed, "too old reads as not installed");
    assert_eq!(
        reason_of(Provider::Utm, true, &old_plugin),
        Some(Unavailable::PluginTooOld {
            plugin: "vagrant_utm".into(),
            version: "0.1.6".into(),
            min: "0.2.0".into()
        })
    );
    // A UTM the plugin refuses, said before the plugin (updating the plugin wouldn't help).
    assert_eq!(
        reason_of(Provider::Utm, true, &utm(Some("4.5.1"), Some("0.1.6"))),
        Some(Unavailable::HypervisorTooOld {
            name: "UTM".into(),
            version: "4.5.1".into(),
            min: "4.6.5".into()
        })
    );
    // Recent enough, or no version to judge: nothing stands in the way.
    assert_eq!(reason_of(Provider::Utm, true, &utm(Some("4.7.5"), Some("0.2.0"))), None);
    assert_eq!(reason_of(Provider::Utm, true, &utm(None, None)), None);
    // Other providers aren't held to a version.
    assert_eq!(plugin_too_old(Provider::Qemu, Some("0.1.0")), None);
    assert_eq!(hypervisor_too_old(Provider::Virtualbox, Some("6.0")), None);
}

/// A VMware Desktop finding: Fusion or Workstation installed, the plugin and the utility as given.
fn vmware(plugin_installed: bool, utility: Option<UtilityState>) -> Findings {
    Findings {
        plugin: Some("vagrant-vmware-desktop"),
        plugin_installed,
        utility,
        ..installed()
    }
}

#[test]
fn a_provider_is_ready_exactly_when_nothing_is_needed() {
    // Readiness and a setup's steps (which install each of `needs`) agree on every combination,
    // so a setup that installs everything it lists leaves the provider ready, and one that is
    // ready lists nothing to install.
    for vagrant in [false, true] {
        for plugin_installed in [false, true] {
            for utility in [None, Some(UtilityState::Missing), Some(UtilityState::Stopped), Some(UtilityState::Running)] {
                let found = vmware(plugin_installed, utility);
                let status = status_from(Provider::VmwareDesktop, vagrant, &found);
                assert_eq!(status.available, status.needs.is_empty(), "{status:?}");
                assert_eq!(status.available, status.reason.is_none(), "{status:?}");
            }
        }
    }
}

#[test]
fn vmware_needs_the_utility_running_as_well_as_its_plugin() {
    // The plugin alone isn't enough (a setup put the plugin in, VMs still couldn't run).
    assert_eq!(
        reason_of(Provider::VmwareDesktop, true, &vmware(true, Some(UtilityState::Missing))),
        Some(Unavailable::VmwareUtilityMissing)
    );
    assert_eq!(
        reason_of(Provider::VmwareDesktop, true, &vmware(true, Some(UtilityState::Stopped))),
        Some(Unavailable::VmwareUtilityStopped)
    );
    // The utility first: it is what a setup installs first (the plugin without it breaks
    // Vagrant for every provider); the plugin once the utility runs.
    assert_eq!(
        reason_of(Provider::VmwareDesktop, true, &vmware(false, Some(UtilityState::Missing))),
        Some(Unavailable::VmwareUtilityMissing)
    );
    assert_eq!(
        reason_of(Provider::VmwareDesktop, true, &vmware(false, Some(UtilityState::Running))),
        Some(Unavailable::PluginMissing {
            plugin: "vagrant-vmware-desktop".into()
        })
    );
    assert_eq!(reason_of(Provider::VmwareDesktop, true, &vmware(true, Some(UtilityState::Running))), None);
}
