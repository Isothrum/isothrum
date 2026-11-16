use std::path::Path;
use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply, Script};

use super::*;
use crate::host::Devices;
use isothrum_runner::ProcessRunner;
use isothrum_test_support::assert_every;

/// Detection on hosts the tests build, their tools scripted strictly.
mod detection;
/// Why a provider is unavailable: the first missing layer, from what detection found.
mod reasons;

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
