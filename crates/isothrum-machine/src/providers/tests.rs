use std::path::Path;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply, Script};

use super::*;
use crate::host::Devices;
use isothrum_runner::ProcessRunner;
use isothrum_test_support::assert_every;

/// Detection on hosts the tests build, their tools scripted strictly.
mod detection;

/// A host on `os` and `arch` whose devices are under `root` (none there until a test makes it).
fn host(os: HostOs, arch: Arch, root: &Path) -> Host {
    Host {
        os,
        arch,
        packages: None,
        devices: Devices::under(root),
    }
}

/// A Linux x86-64 host whose devices are under `root`, with a KVM device the user may open:
/// VirtualBox, VMware Workstation, libvirt, QEMU and ESXi apply to it.
fn linux(root: &Path) -> Host {
    let host = host(HostOs::Linux, Arch::Amd64, root);
    write(&host.devices.kvm, "");
    host
}

/// Writes `text` to `path`, making its folder.
fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Machine setup on `host`, its tools `tools`, which the test keeps to read the calls made.
fn context(tools: FakeProcesses, host: Host) -> (Context, Arc<FakeProcesses>) {
    let tools = Arc::new(tools);
    let runner: Arc<dyn ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    (Context::new(runner, host), tools)
}

/// A Linux host's hypervisor probes, each hypervisor missing unless the script answers it
/// already, and `id` not answering (no group joined since logging in); `script` names what the
/// test is about.
fn on_linux(script: Script) -> Script {
    script
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("virsh", Reply::missing())
        .allow("qemu-system-x86_64", Reply::missing())
        .allow("id", Reply::missing())
}

/// The status `detect` gave `provider`; it must have given one.
fn status_of(statuses: &[ProviderStatus], provider: Provider) -> &ProviderStatus {
    statuses
        .iter()
        .find(|s| s.provider == provider)
        .unwrap_or_else(|| panic!("no {provider:?} in {statuses:?}"))
}

/// The providers `statuses` are of, in their order.
fn providers(statuses: &[ProviderStatus]) -> Vec<Provider> {
    statuses.iter().map(|s| s.provider).collect()
}

const PLUGINS: &str = "vagrant plugin list --machine-readable";

/// `vagrant plugin list --machine-readable`'s answer listing `plugins`, each at its version.
fn listing(plugins: &[(&str, &str)]) -> Reply {
    let lines: Vec<String> = plugins
        .iter()
        .map(|(name, version)| format!("1,,plugin-name,{name}\n1,{name},plugin-version,{version}%!(VAGRANT_COMMA) global"))
        .collect();
    Reply::ok(&lines.join("\n"))
}

/// Detection covers every provider the type names: one added there fails this until it is
/// listed.
#[test]
fn every_provider_is_listed_with_the_id_vagrant_uses() {
    let ids: Vec<String> = all().iter().map(isothrum_runner::wire_name).collect();
    assert_eq!(
        ids,
        ["virtualbox", "vmware_desktop", "hyperv", "parallels", "libvirt", "qemu", "utm", "vmware_esxi"]
    );
    let mut every = isothrum_runner::every_name::<Provider>();
    every.sort();
    let mut listed = ids.clone();
    listed.sort();
    assert_eq!(listed, every);
}

/// Each provider on each OS and CPU, not only this host's.
#[test]
fn a_provider_applies_where_its_hypervisor_runs() {
    let platforms = [
        (HostOs::Linux, Arch::Amd64),
        (HostOs::Linux, Arch::Arm64),
        (HostOs::Macos, Arch::Arm64),
        (HostOs::Macos, Arch::Amd64),
        (HostOs::Windows, Arch::Amd64),
        (HostOs::Windows, Arch::Arm64),
    ];
    let table = [
        (Provider::Virtualbox, [true, true, true, true, true, false]),
        (Provider::VmwareDesktop, [true, true, true, true, true, false]),
        (Provider::Hyperv, [false, false, false, false, true, true]),
        (Provider::Parallels, [false, false, true, true, false, false]),
        (Provider::Utm, [false, false, true, true, false, false]),
        (Provider::Libvirt, [true, true, false, false, false, false]),
        // Regression (#314): QEMU was offered on Windows, where no VM runs on it.
        (Provider::Qemu, [true, true, true, true, false, false]),
        (Provider::VmwareEsxi, [true; 6]),
    ];
    assert_eq!(table.len(), all().len(), "every provider is in the table");
    let root = Path::new("unused");
    for (provider, expected) in table {
        for ((os, arch), applies) in platforms.iter().zip(expected) {
            assert_eq!(applicable(provider, &host(*os, *arch, root)), applies, "{provider:?} on {os:?} {arch:?}");
        }
    }
}

#[test]
fn a_mac_is_offered_the_hypervisor_made_for_its_cpu_and_keeps_one_installed() {
    let root = Path::new("unused");
    let (apple, intel) = (host(HostOs::Macos, Arch::Arm64, root), host(HostOs::Macos, Arch::Amd64, root));
    assert!(!offered(Provider::Virtualbox, &apple, false));
    assert!(offered(Provider::Utm, &apple, false));
    assert!(offered(Provider::Virtualbox, &intel, false));
    assert!(!offered(Provider::Utm, &intel, false));
    assert!(offered(Provider::Virtualbox, &apple, true), "installed: the user chose it");
    assert!(offered(Provider::Virtualbox, &host(HostOs::Linux, Arch::Arm64, root), false));
    assert!(
        !offered(Provider::Utm, &host(HostOs::Linux, Arch::Arm64, root), true),
        "never where it doesn't apply"
    );
    assert!(!offered(Provider::Hyperv, &apple, true));
}

#[test]
fn the_hypervisor_table_says_where_each_provider_shares_folders_and_where_its_vendor_offers_it() {
    let esxi = ProviderRow::of(Provider::VmwareEsxi);
    assert!(esxi.remote() && esxi.shares_folders.is_empty());
    assert_every!(all().into_iter().filter(|p| *p != Provider::VmwareEsxi), |p: &Provider| !ProviderRow::of(*p)
        .remote());
    assert!(
        ProviderRow::of(Provider::Hyperv).shares_folders.is_empty(),
        "SMB folders ask for a password at every start"
    );
    assert_eq!(ProviderRow::of(Provider::Qemu).shares_folders, [HostOs::Linux]);
    for p in [Provider::VmwareDesktop, Provider::Hyperv, Provider::Parallels] {
        assert!(ProviderRow::of(p).vendor.is_some_and(|url| url.starts_with("https://")), "{p:?}");
    }
    assert_eq!(ProviderRow::of(Provider::Utm).plugin_minimum, Some("0.2.0"));
    assert_eq!(ProviderRow::of(Provider::Utm).hypervisor_minimum, Some(("UTM", "4.6.5")));
}

#[test]
fn only_qemu_on_linux_needs_mkisofs_and_only_vmware_desktop_the_utility() {
    assert!(needs_mkisofs(Provider::Qemu, HostOs::Linux));
    for (p, os) in [
        (Provider::Qemu, HostOs::Macos),
        (Provider::Libvirt, HostOs::Linux),
        (Provider::Virtualbox, HostOs::Linux),
    ] {
        assert!(!needs_mkisofs(p, os), "{p:?} on {os:?}");
    }
    assert!(needs_vmware_utility(Provider::VmwareDesktop));
    assert_every!(all().into_iter().filter(|p| *p != Provider::VmwareDesktop), |p: &Provider| {
        !needs_vmware_utility(*p)
    });
}

#[test]
fn a_hypervisors_probe_is_its_tool_and_qemus_is_for_the_cpu() {
    assert_eq!(probe_of(Provider::Virtualbox, Arch::Amd64).unwrap().0, "VBoxManage");
    assert_eq!(probe_of(Provider::VmwareDesktop, Arch::Amd64).unwrap().0, "vmrun");
    assert_eq!(probe_of(Provider::Parallels, Arch::Arm64).unwrap().0, "prlctl");
    assert_eq!(probe_of(Provider::Libvirt, Arch::Amd64).unwrap().0, "virsh");
    assert_eq!(probe_of(Provider::Qemu, Arch::Arm64).unwrap().0, "qemu-system-aarch64");
    assert_eq!(probe_of(Provider::Qemu, Arch::Amd64).unwrap().0, "qemu-system-x86_64");
    for none in [Provider::Hyperv, Provider::Utm, Provider::VmwareEsxi] {
        assert!(probe_of(none, Arch::Amd64).is_none(), "{none:?}");
    }
}

/// Findings of an installed hypervisor that needs nothing else.
fn installed() -> Findings {
    Findings {
        hypervisor: Some(true),
        plugin_installed: true,
        ..Findings::default()
    }
}

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
        mkisofs_missing: true,
        program: Some("qemu-system-x86_64"),
        plugin: Some("vagrant-qemu"),
        ..installed()
    };
    assert_eq!(reason_of(Provider::Qemu, true, &qemu), Some(Unavailable::MkisofsMissing));
    assert_eq!(reason_of(Provider::Qemu, false, &qemu), Some(Unavailable::VagrantMissing));
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
    // The plugin first: it is what a setup installs first.
    assert_eq!(
        reason_of(Provider::VmwareDesktop, true, &vmware(false, Some(UtilityState::Missing))),
        Some(Unavailable::PluginMissing {
            plugin: "vagrant-vmware-desktop".into()
        })
    );
    assert_eq!(reason_of(Provider::VmwareDesktop, true, &vmware(true, Some(UtilityState::Running))), None);
}

#[test]
fn a_status_serializes_in_camel_case_with_its_reason_as_a_code() {
    let status = status_from(
        Provider::Libvirt,
        true,
        &Findings {
            plugin: Some("vagrant-libvirt"),
            plugin_installed: false,
            account: Some(Account::NewLogin("libvirt".into())),
            ..installed()
        },
    );
    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(
        wire,
        serde_json::json!({
            "provider": "libvirt",
            "remote": false,
            "available": false,
            "hypervisor": true,
            "plugin": "vagrant-libvirt",
            "pluginInstalled": false,
            "utility": null,
            "needs": ["plugin"],
            "reason": { "code": "newLogin", "args": { "group": "libvirt" } },
            "accountLacks": "newLogin",
        })
    );
    assert_eq!(serde_json::from_value::<ProviderStatus>(wire).unwrap(), status);
}
