use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;

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

#[tokio::test]
async fn detect_reports_a_ready_qemu_and_an_unavailable_one_by_its_plugin() {
    // QEMU installed, its plugin present: available. A missing plugin makes it unavailable.
    let with = FakeProcesses::rules(&[
        ("qemu-system", Reply::ok("QEMU 10.0")),
        (
            "vagrant plugin list --machine-readable",
            Reply::ok("1,,plugin-name,vagrant-qemu\n1,vagrant-qemu,plugin-version,0.6.3%!(VAGRANT_COMMA) global"),
        ),
    ]);
    let qemu = detect(&with, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
    assert!(qemu.reason.is_none(), "{qemu:?}");

    let without = FakeProcesses::rules(&[("qemu-system", Reply::ok("QEMU 10.0")), ("vagrant plugin list", Reply::ok(""))]);
    let no_plugin = detect(&without, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
    assert_eq!(no_plugin.reason.as_deref(), Some("Vagrant plugin `vagrant-qemu` is not installed"),);
}

#[tokio::test]
async fn a_vagrant_that_cant_list_its_plugins_is_one_reason_not_each_plugin_missing() {
    let broken = FakeProcesses::rules(&[
        ("qemu-system", Reply::ok("QEMU 10.0")),
        ("VBoxManage", Reply::ok("7.1.4")),
        ("vagrant plugin list", Reply::fail(1, "Vagrant failed to initialize at a very early stage")),
    ]);
    let statuses = detect(&broken, true).await;
    let qemu = statuses.iter().find(|s| s.provider == Provider::Qemu).unwrap();
    assert_eq!(
        qemu.reason.as_deref(),
        Some("Vagrant couldn't say which plugins it has (`vagrant plugin list`: Vagrant failed to initialize at a very early stage (exited with code 1))")
    );
    // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
    let virtualbox = statuses.iter().find(|s| s.provider == Provider::Virtualbox);
    assert!(virtualbox.is_none_or(|s| s.reason.is_none()), "{virtualbox:?}");
}

/// Each missing layer is said for what it is: Hyper-V turned off (a Windows feature, not a
/// program), a hypervisor whose program isn't installed.
#[test]
fn a_hypervisor_that_is_off_or_not_installed_says_which() {
    let absent = Detection {
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

#[tokio::test]
async fn a_virtualbox_that_isnt_installed_is_detected_as_such() {
    let tools = FakeProcesses::rules(&[("VBoxManage", Reply::missing())]);
    let statuses = detect(&tools, true).await;
    let virtualbox = statuses.iter().find(|s| s.provider == Provider::Virtualbox);
    assert!(
        virtualbox.is_none_or(|s| s.reason.as_deref() == Some("`VBoxManage` was not found")),
        "{virtualbox:?}"
    );
}

#[tokio::test]
async fn without_vagrant_every_provider_says_so_and_nothing_is_probed() {
    let tools = FakeProcesses::rules(&[]);
    let statuses = detect(&tools, false).await;
    assert_eq!(statuses.len(), all().iter().filter(|p| applicable(**p)).count());
    assert!(statuses.iter().all(|s| s.reason.as_deref() == Some(NO_VAGRANT)), "{statuses:?}");
    assert_eq!(tools.calls(), [], "no probe runs without Vagrant");
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

#[tokio::test(start_paused = true)]
async fn a_virtualbox_whose_service_never_answers_is_wedged() {
    assert!(vbox_wedged(&Stuck).await);
    let answering = FakeProcesses::rules(&[("VBoxManage list runningvms", Reply::ok(""))]);
    assert!(!vbox_wedged(&answering).await);
}

#[tokio::test]
async fn only_utm_is_asked_its_version() {
    let tools = FakeProcesses::rules(&[("defaults read", Reply::ok("4.7.5\n"))]);
    assert_eq!(hypervisor_version(Provider::Utm, &tools).await.as_deref(), Some("4.7.5"));
    assert_eq!(hypervisor_version(Provider::Qemu, &tools).await, None);
    let silent = FakeProcesses::rules(&[("defaults read", Reply::ok(""))]);
    assert_eq!(hypervisor_version(Provider::Utm, &silent).await, None, "no version said, none read");
    let call = tools.calls().into_iter().next().unwrap();
    assert_eq!(call.args, ["read", "/Applications/UTM.app/Contents/Info.plist", "CFBundleShortVersionString"]);
}

#[tokio::test]
async fn an_installed_hyperv_needs_admin_only_when_whoami_says_the_account_may_not_manage_it() {
    let groups = |sid: &str| format!("\"Label\",\"Label\",\"{sid}\",");
    let not_allowed = FakeProcesses::rules(&[("whoami /groups", Reply::ok(&groups("S-1-16-8192")))]);
    let allowed = FakeProcesses::rules(&[("whoami /groups", Reply::ok(&groups("S-1-5-32-578")))]);
    let unknown = FakeProcesses::rules(&[("whoami", Reply::missing())]);
    assert!(needs_admin(Provider::Hyperv, Some(true), &not_allowed).await);
    assert!(!needs_admin(Provider::Hyperv, Some(true), &allowed).await);
    assert!(!needs_admin(Provider::Hyperv, Some(true), &unknown).await, "no answer adds no reason");
    assert!(!needs_admin(Provider::Hyperv, Some(false), &not_allowed).await, "not installed says so instead");
    assert!(!needs_admin(Provider::Virtualbox, Some(true), &not_allowed).await);
    assert_eq!(not_allowed.calls().len(), 1, "whoami is asked for an installed Hyper-V only");
}

#[test]
fn hyperv_needs_admin_folds_into_its_reason() {
    // Hyper-V's `applicable()` gate keeps it out of `detect` off Windows, so `needs_admin` is
    // tested on its own (above) and through `unavailable_reason` here.
    let hyperv = |needs_admin| Detection {
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
#[tokio::test]
async fn a_virtualbox_whose_kernel_driver_isnt_loaded_isnt_ready() {
    let oracle = "WARNING: The vboxdrv kernel module is not loaded. Either there is no module\n         available for the current kernel (6.8.0-49-generic) or it failed to\n         load. Please recompile the kernel module and install it by\n\n           sudo /sbin/vboxconfig\n\n         You will not be able to start VMs until this problem is fixed.\n7.0.14r161095";
    let distribution = "WARNING: The character device /dev/vboxdrv does not exist.\n\t Please install the virtualbox-dkms package and the appropriate\n\t headers, most likely linux-headers-6.8.0-49-generic.\n\n\t You will not be able to start VMs until this problem is fixed.\n7.0.16_Ubuntur162802";
    for said in [oracle, distribution] {
        assert!(vbox_driver_missing(said), "{said}");
        let tools = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok(said))]);
        assert_eq!(vbox_driver_problem(&tools).await.as_deref(), Some(VBOX_DRIVER_MISSING));
        // Printed on standard error, it is read too.
        let mut on_stderr = Reply::ok("7.0.14r161095");
        on_stderr.stderr = said.lines().map(str::to_owned).collect();
        let warned_on_stderr = FakeProcesses::rules(&[("VBoxManage --version", on_stderr)]);
        assert_eq!(vbox_driver_problem(&warned_on_stderr).await.as_deref(), Some(VBOX_DRIVER_MISSING));
    }
    let fine = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok("7.1.4r165100"))]);
    assert_eq!(vbox_driver_problem(&fine).await, None);
    if applicable(Provider::Virtualbox) {
        let detected = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok(oracle)), ("vagrant plugin list", Reply::ok(""))]);
        let virtualbox = detect(&detected, true).await.into_iter().find(|s| s.provider == Provider::Virtualbox).unwrap();
        assert_eq!(virtualbox.reason.as_deref(), Some(VBOX_DRIVER_MISSING));
    }
}

/// Regression (#135): Hyper-V turned on with its hypervisor off (`hypervisorlaunchtype off`,
/// virtualization off in the firmware) read as ready, and `vagrant up` failed minutes in.
#[tokio::test]
async fn an_installed_hyperv_whose_hypervisor_isnt_running_isnt_ready() {
    let said = |answer: Reply| FakeProcesses::rules(&[("powershell", answer)]);
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
