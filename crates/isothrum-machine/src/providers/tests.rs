use isothrum_runner::fake::{FakeProcesses, Reply, Script};

use super::*;

/// Detection on a host the test gives, its tools scripted strictly.
mod detection;

/// A Linux amd64 host whose KVM device `kvm` the user may open: VirtualBox, VMware
/// Workstation, libvirt, QEMU and ESXi apply to it.
fn linux(kvm: &Path) -> Host {
    std::fs::write(kvm, "").unwrap();
    Host {
        os: HostOs::Linux,
        arch: Arch::Amd64,
        kvm: kvm.to_path_buf(),
        utm_installed: false,
        hyperv_enabled: false,
    }
}

/// A Linux host's hypervisor probes, each hypervisor missing unless the script answers it
/// already; `script` names what the test is about.
fn on_linux(script: Script) -> Script {
    script
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("virsh", Reply::missing())
        .allow("qemu-system-x86_64", Reply::missing())
}

/// The status `detect` gave `provider`; it must have given one.
fn status_of(statuses: &[ProviderStatus], provider: Provider) -> &ProviderStatus {
    statuses
        .iter()
        .find(|s| s.provider == provider)
        .unwrap_or_else(|| panic!("no {provider:?} in {statuses:?}"))
}

const PLUGINS: &str = "vagrant plugin list --machine-readable";

/// The names Isoloom's `Provider` takes, sorted.
fn isoloom_providers() -> Vec<String> {
    let mut names = isothrum_runner::every_name::<Provider>();
    names.sort();
    names
}

/// Detection covers every provider Isoloom names: one added there fails this until it is
/// listed.
#[test]
fn every_provider_isoloom_names_is_listed() {
    let mut listed: Vec<String> = all().iter().map(isothrum_runner::wire_name).collect();
    listed.sort();
    assert_eq!(listed, isoloom_providers());
}

/// The problem of a device the user may not open: one only its owner reads (root reads any,
/// so as root the text is the one it would be).
fn denied_device(dir: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let device = dir.join("denied");
        std::fs::write(&device, "").unwrap();
        std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o400)).unwrap();
        if let Some(said) = kvm_problem(&device) {
            assert!(said.starts_with("you can't use /dev/kvm yet"), "{said}");
            return said;
        }
    }
    let _ = dir;
    "you can't use /dev/kvm yet: add yourself to the kvm and libvirt groups, then sign out and back in".to_owned()
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
        (Provider::Qemu, [true; 6]),
        (Provider::VmwareEsxi, [true; 6]),
    ];
    assert_eq!(table.len(), all().len(), "every provider is in the table");
    for (provider, expected) in table {
        for ((os, arch), applies) in platforms.iter().zip(expected) {
            assert_eq!(applicable_on(provider, *os, *arch), applies, "{provider:?} on {os:?} {arch:?}");
        }
    }
    assert_eq!(qemu_binary_on(Arch::Arm64), "qemu-system-aarch64");
    assert_eq!(qemu_binary_on(Arch::Amd64), "qemu-system-x86_64");
}

#[test]
fn a_providers_id_is_the_one_vagrant_and_the_manifest_use() {
    let ids: Vec<String> = all().iter().map(isothrum_runner::wire_name).collect();
    assert_eq!(
        ids,
        ["virtualbox", "vmware_desktop", "hyperv", "parallels", "libvirt", "qemu", "utm", "vmware_esxi"]
    );
}

#[test]
fn utm_and_its_plugin_are_held_to_a_version() {
    let detection = |hv: Option<&'static str>, pl: Option<&'static str>, installed: bool| Detection {
        arch: Arch::Amd64,
        hypervisor: Some(true),
        hypervisor_version: hv,
        unresponsive: false,
        plugin: Some("vagrant_utm"),
        plugin_installed: installed,
        plugin_version: pl,
        plugins_unknown: None,
        needs_admin: false,
        platform_problem: None,
    };
    assert_eq!(plugin_too_old(Provider::Utm, Some("0.1.6")), Some(("0.1.6", "0.2.0")));
    // Regression: a suffix was dropped, so a pre-release of the minimum passed for it; it comes
    // before its release, as the runner's tool probes (Isoloom's Version) order it.
    assert_eq!(plugin_too_old(Provider::Utm, Some("0.2.0-beta")), Some(("0.2.0-beta", "0.2.0")));
    assert_eq!(plugin_too_old(Provider::Utm, Some("unknown")), None, "no version to judge");
    assert_eq!(
        unavailable_reason(Provider::Utm, detection(Some("4.7.5"), Some("0.1.6"), true)).as_deref(),
        Some("Vagrant plugin `vagrant_utm` 0.1.6 is too old: 0.2.0 or later is needed. Install it again to update it")
    );
    // A UTM the plugin refuses, said before the plugin (updating the plugin wouldn't help).
    assert_eq!(
        unavailable_reason(Provider::Utm, detection(Some("4.5.1"), Some("0.1.6"), true)).as_deref(),
        Some("UTM 4.5.1 is too old: 4.6.5 or later is needed. Update UTM")
    );
    // Recent enough, or no version to judge: nothing stands in the way.
    assert_eq!(unavailable_reason(Provider::Utm, detection(Some("4.7.5"), Some("0.2.0"), true)), None);
    assert_eq!(unavailable_reason(Provider::Utm, detection(None, None, true)), None);
    assert_eq!(plugin_too_old(Provider::Utm, None), None);
    // Other providers aren't held to a version.
    assert_eq!(plugin_too_old(Provider::Qemu, Some("0.1.0")), None);
    assert_eq!(hypervisor_too_old(Provider::Virtualbox, Some("6.0")), None);
}

/// Each missing layer is said for what it is: Hyper-V turned off (a Windows feature, not a
/// program), a hypervisor whose program isn't installed.
#[test]
fn a_hypervisor_that_is_off_or_not_installed_says_which() {
    let absent = Detection {
        arch: Arch::Amd64,
        hypervisor: Some(false),
        hypervisor_version: None,
        unresponsive: false,
        plugin: None,
        plugin_installed: true,
        plugin_version: None,
        plugins_unknown: None,
        needs_admin: false,
        platform_problem: None,
    };
    assert_eq!(
        unavailable_reason(Provider::Hyperv, absent).as_deref(),
        Some("Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it")
    );
    assert_eq!(unavailable_reason(Provider::Virtualbox, absent).as_deref(), Some("`VBoxManage` was not found"));
}

/// libvirt needs KVM's device, and the user's leave to open it: each is said, read once by
/// `detect`, and comes after the tool itself (no `virsh` is said first).
#[test]
fn libvirt_without_kvm_or_its_leave_says_which() {
    let dir = tempfile::tempdir().unwrap();
    let missing = kvm_problem(&dir.path().join("kvm")).unwrap();
    assert!(missing.starts_with("KVM isn't available on this machine (no /dev/kvm)"), "{missing}");
    // Regression: any failure to open it (a folder here; EBUSY, ENXIO on a real device) was
    // said as a device the user may not open. Unix only: Windows refuses to open a folder
    // with "access denied", the permission problem's own kind.
    #[cfg(unix)]
    {
        let other = kvm_problem(dir.path()).unwrap();
        assert!(other.starts_with("/dev/kvm can't be opened: "), "{other}");
    }
    let device = dir.path().join("device");
    std::fs::write(&device, "").unwrap();
    assert_eq!(kvm_problem(&device), None);
    let denied = denied_device(dir.path());
    let (missing, denied) = (missing.as_str(), denied.as_str());
    let libvirt = |hypervisor, platform_problem| Detection {
        arch: Arch::Amd64,
        hypervisor: Some(hypervisor),
        hypervisor_version: None,
        unresponsive: false,
        plugin: Some("vagrant-libvirt"),
        plugin_installed: true,
        plugin_version: None,
        plugins_unknown: None,
        needs_admin: false,
        platform_problem,
    };
    assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, Some(missing))).as_deref(), Some(missing));
    assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, Some(denied))).as_deref(), Some(denied));
    assert_eq!(
        unavailable_reason(Provider::Libvirt, libvirt(false, Some(missing))).as_deref(),
        Some("`virsh` was not found")
    );
    assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, None)), None);
}

/// A tool that never answers.
struct Stuck;

impl ProcessRunner for Stuck {
    fn run<'a>(
        &'a self,
        _spec: &'a isothrum_runner::ProcessSpec,
        _on_line: isothrum_runner::OnLine<'a>,
    ) -> isothrum_runner::BoxFuture<'a, Result<isothrum_runner::Exit, isothrum_runner::ProcessError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(isothrum_runner::Exit::new(Some(0)))
        })
    }
}

#[test]
fn hyperv_needs_admin_folds_into_its_reason() {
    let hyperv = |needs_admin| Detection {
        arch: Arch::Amd64,
        hypervisor: Some(true),
        hypervisor_version: None,
        unresponsive: false,
        plugin: None,
        plugin_installed: true,
        plugin_version: None,
        plugins_unknown: None,
        needs_admin,
        platform_problem: None,
    };
    assert_eq!(unavailable_reason(Provider::Hyperv, hyperv(true)).as_deref(), Some(HYPERV_NEEDS_ADMIN));
    assert_eq!(unavailable_reason(Provider::Hyperv, hyperv(false)), None);
}

/// Regression (#128): with its kernel driver not loaded, `VBoxManage` still answers, after a
/// warning, and VirtualBox read as ready until `vagrant up` failed.
#[tokio::test(start_paused = true)]
async fn a_virtualbox_whose_kernel_driver_isnt_loaded_isnt_ready() {
    let oracle = "WARNING: The vboxdrv kernel module is not loaded. Either there is no module\n         available for the current kernel (6.8.0-49-generic) or it failed to\n         load. Please recompile the kernel module and install it by\n\n           sudo /sbin/vboxconfig\n\n         You will not be able to start VMs until this problem is fixed.\n7.0.14r161095";
    let distribution = "WARNING: The character device /dev/vboxdrv does not exist.\n\t Please install the virtualbox-dkms package and the appropriate\n\t headers, most likely linux-headers-6.8.0-49-generic.\n\n\t You will not be able to start VMs until this problem is fixed.\n7.0.16_Ubuntur162802";
    for said in [oracle, distribution] {
        assert!(vbox_driver_missing(said), "{said}");
        let tools = FakeProcesses::expect(&[("VBoxManage --version", Reply::ok(said))]);
        assert_eq!(vbox_driver_problem(&tools).await.as_deref(), Some(VBOX_DRIVER_MISSING));
        // Printed on standard error, it is read too.
        let mut on_stderr = Reply::ok("7.0.14r161095");
        on_stderr.stderr = said.lines().map(str::to_owned).collect();
        let warned_on_stderr = FakeProcesses::expect(&[("VBoxManage --version", on_stderr)]);
        assert_eq!(vbox_driver_problem(&warned_on_stderr).await.as_deref(), Some(VBOX_DRIVER_MISSING));
    }
    let fine = FakeProcesses::expect(&[("VBoxManage --version", Reply::ok("7.1.4r165100"))]);
    assert_eq!(vbox_driver_problem(&fine).await, None);
    // Through detection, on a Linux host whatever the machine running the test.
    let dir = tempfile::tempdir().unwrap();
    let detected = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::ok(oracle))
        .allow("VBoxManage list runningvms", Reply::ok(""))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &detected, true).await;
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason.as_deref(), Some(VBOX_DRIVER_MISSING));
}

/// Regression (#135): Hyper-V turned on with its hypervisor off (`hypervisorlaunchtype off`,
/// virtualization off in the firmware) read as ready, and `vagrant up` failed minutes in.
#[tokio::test(start_paused = true)]
async fn an_installed_hyperv_whose_hypervisor_isnt_running_isnt_ready() {
    let said = |answer: Reply| FakeProcesses::expect(&[("powershell", answer)]);
    assert_eq!(hypervisor_stopped(&said(Reply::ok("False\r\n"))).await.as_deref(), Some(HYPERVISOR_STOPPED));
    assert_eq!(hypervisor_stopped(&said(Reply::ok("True"))).await, None);
    assert_eq!(
        hypervisor_stopped(&said(Reply::fail(1, "Get-CimInstance : Access denied"))).await,
        None,
        "unknown adds no reason"
    );
    assert_eq!(hypervisor_stopped(&said(Reply::missing())).await, None);
    let asked = said(Reply::ok("False"));
    hypervisor_stopped(&asked).await;
    assert_eq!(asked.calls()[0].args.last().map(String::as_str), Some(HYPERVISOR_PRESENT));
    // Folded into Hyper-V's status after the layers before it.
    let off = Detection {
        arch: Arch::Amd64,
        hypervisor: Some(true),
        hypervisor_version: None,
        unresponsive: false,
        plugin: None,
        plugin_installed: true,
        plugin_version: None,
        plugins_unknown: None,
        needs_admin: false,
        platform_problem: Some(HYPERVISOR_STOPPED),
    };
    assert_eq!(unavailable_reason(Provider::Hyperv, off).as_deref(), Some(HYPERVISOR_STOPPED));
}
