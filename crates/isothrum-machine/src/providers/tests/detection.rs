use isothrum_runner::fake::{FakeProcesses, Reply};
use isothrum_test_support::assert_every;

use std::time::Duration;

use super::*;
use crate::testing::{Stuck, StuckOn};

/// Detection, Vagrant installed, on a Linux x86-64 host whose KVM device the user may open, its
/// tools `tools`.
async fn detect_on_linux(tools: FakeProcesses) -> Vec<ProviderStatus> {
    let dir = tempfile::tempdir().unwrap();
    let (cx, _) = context(tools, linux(dir.path()));
    detect(&cx, true).await
}

// Linux.

#[tokio::test(start_paused = true)]
async fn a_qemu_with_its_plugin_and_mkisofs_is_ready() {
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("mkisofs -version", Reply::ok("mkisofs 2.01"))
        .expect(PLUGINS, listing(&[("vagrant-qemu", "0.6.3")]))
        .build();
    let statuses = detect_on_linux(tools).await;
    let qemu = status_of(&statuses, Provider::Qemu);
    assert_eq!((qemu.reason.clone(), qemu.available, qemu.hypervisor), (None, true, Some(true)));
    assert!(qemu.needs.is_empty() && qemu.plugin_installed, "{qemu:?}");
}

#[tokio::test(start_paused = true)]
async fn qemu_on_linux_reads_as_not_installed_without_mkisofs_which_its_install_brings() {
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("mkisofs -version", Reply::missing())
        .expect(PLUGINS, listing(&[("vagrant-qemu", "0.6.3")]))
        .build();
    let statuses = detect_on_linux(tools).await;
    let qemu = status_of(&statuses, Provider::Qemu);
    assert_eq!((qemu.hypervisor, qemu.reason.clone()), (Some(false), Some(Unavailable::MkisofsMissing)));
}

#[tokio::test(start_paused = true)]
async fn a_qemu_without_its_plugin_says_so_and_needs_it() {
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("mkisofs -version", Reply::ok("mkisofs 2.01"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on_linux(tools).await;
    let qemu = status_of(&statuses, Provider::Qemu);
    assert_eq!(qemu.reason, Some(Unavailable::PluginMissing { plugin: "vagrant-qemu".into() }));
    assert_eq!(qemu.needs, [Need::Plugin]);
}

#[tokio::test(start_paused = true)]
async fn an_arm_host_probes_its_own_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host {
        arch: Arch::Aarch64,
        ..linux(dir.path())
    };
    let tools = FakeProcesses::script()
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("virsh", Reply::missing())
        .expect("qemu-system-aarch64 --version", Reply::missing())
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, true).await;
    assert_eq!(
        status_of(&statuses, Provider::Qemu).reason,
        Some(Unavailable::NotInstalled {
            program: Some("qemu-system-aarch64".into())
        })
    );
}

#[tokio::test(start_paused = true)]
async fn a_vagrant_that_cant_list_its_plugins_is_one_reason_not_each_plugin_missing() {
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("mkisofs -version", Reply::ok("mkisofs 2.01"))
        .expect("VBoxManage --version", Reply::ok("7.1.4"))
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect(PLUGINS, Reply::fail(1, "Vagrant failed to initialize at a very early stage"))
        .build();
    let statuses = detect_on_linux(tools).await;
    assert_eq!(
        status_of(&statuses, Provider::Qemu).reason,
        Some(Unavailable::PluginsUnknown {
            plugin: "vagrant-qemu".into(),
            reason: "Vagrant failed to initialize at a very early stage (exited with code 1)".into()
        })
    );
    // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason, None);
}

#[tokio::test(start_paused = true)]
async fn a_virtualbox_that_isnt_installed_is_detected_as_such() {
    let tools = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::missing())
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on_linux(tools).await;
    let virtualbox = status_of(&statuses, Provider::Virtualbox);
    assert_eq!(
        (virtualbox.hypervisor, virtualbox.reason.clone()),
        (
            Some(false),
            Some(Unavailable::NotInstalled {
                program: Some("VBoxManage".into())
            })
        )
    );
}

/// Regression: a probe that ran out of time (a cold `VBoxSVC`) read as VirtualBox missing, and
/// setup offered to install it again.
#[tokio::test(start_paused = true)]
async fn a_hypervisor_whose_probe_runs_out_of_time_is_installed_and_slow() {
    let dir = tempfile::tempdir().unwrap();
    let script = on_linux(FakeProcesses::script())
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let tools = StuckOn::new("VBoxManage --version", script);
    let cx = Context::new(Arc::new(tools), linux(dir.path())).with_probe_timeout(Duration::from_secs(3));
    let statuses = detect(&cx, true).await;
    let virtualbox = status_of(&statuses, Provider::Virtualbox);
    assert_eq!((virtualbox.hypervisor, virtualbox.reason.clone()), (Some(true), None));
}

#[tokio::test(start_paused = true)]
async fn a_virtualbox_whose_service_never_answers_is_wedged() {
    let dir = tempfile::tempdir().unwrap();
    let stuck = Context::new(Arc::new(Stuck), linux(dir.path()));
    assert!(vbox::wedged(&stuck).await);
    let (answering, _) = context(FakeProcesses::expect(&[("VBoxManage list runningvms", Reply::ok(""))]), linux(dir.path()));
    assert!(!vbox::wedged(&answering).await);
    // Through detection.
    let script = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::ok("7.1.4r165100"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let tools = StuckOn::new("VBoxManage list runningvms", script);
    let cx = Context::new(Arc::new(tools), linux(dir.path()));
    let statuses = detect(&cx, true).await;
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason, Some(Unavailable::VirtualboxStuck));
}

/// Regression (#128): with its kernel driver not loaded, `VBoxManage` still answers, after a
/// warning, and VirtualBox read as ready until `vagrant up` failed. Where the module list can't
/// be read, the warning says it; the kernel is asked of `uname -r`.
#[tokio::test(start_paused = true)]
async fn a_virtualbox_whose_kernel_driver_isnt_loaded_isnt_ready() {
    let oracle = "WARNING: The vboxdrv kernel module is not loaded. Either there is no module\n         available for the current kernel (6.8.0-49-generic) or it failed to\n         load. Please recompile the kernel module and install it by\n\n           sudo /sbin/vboxconfig\n\n         You will not be able to start VMs until this problem is fixed.\n7.0.14r161095";
    let mut on_stderr = Reply::ok("7.0.14r161095");
    on_stderr.stderr = oracle.lines().map(str::to_owned).collect();
    for warned in [Reply::ok(oracle), on_stderr] {
        let tools = on_linux(FakeProcesses::script())
            .expect("VBoxManage --version", warned)
            .expect("VBoxManage list runningvms", Reply::ok(""))
            .expect("uname -r", Reply::ok("6.8.0-49-generic\n"))
            .expect(PLUGINS, Reply::ok(""))
            .build();
        let statuses = detect_on_linux(tools).await;
        assert_eq!(
            status_of(&statuses, Provider::Virtualbox).reason,
            Some(Unavailable::VirtualboxDriver {
                kernel: "6.8.0-49-generic".into()
            })
        );
    }
}

/// The module list, where it can be read, comes first: it tells a restart from a rebuild.
#[tokio::test(start_paused = true)]
async fn a_virtualbox_driver_waiting_for_the_newer_kernel_asks_for_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let host = linux(dir.path());
    write(&host.devices.proc_modules, "kvm 1404928 0 - Live 0x0\n");
    write(&host.devices.kernel_release, "6.12.86+deb13-amd64\n");
    std::fs::create_dir_all(host.devices.lib_modules.join("6.12.116+deb13-amd64/build")).unwrap();
    let tools = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::ok("7.1.4r165100"))
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, true).await;
    assert_eq!(
        status_of(&statuses, Provider::Virtualbox).reason,
        Some(Unavailable::KernelRestart {
            kernel: "6.12.116+deb13-amd64".into()
        })
    );
}

#[tokio::test(start_paused = true)]
async fn kvm_holding_the_cpus_virtualization_is_said_for_virtualbox_installed_or_not() {
    let dir = tempfile::tempdir().unwrap();
    let host = linux(dir.path());
    write(&host.devices.proc_modules, "kvm_intel 413696 0 - Live 0x0\nkvm 1404928 1 kvm_intel, Live 0x0\n");
    write(&host.devices.kvm_virt_at_load, "Y\n");
    let tools = on_linux(FakeProcesses::script()).expect("VBoxManage --version", Reply::missing()).build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, false).await;
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason, Some(Unavailable::KvmHoldsVirtualization));
    // libvirt runs on KVM: it is not held from it.
    assert_eq!(status_of(&statuses, Provider::Libvirt).reason, Some(Unavailable::VagrantMissing));
}

#[tokio::test(start_paused = true)]
async fn libvirt_on_a_host_without_kvm_says_so_once_virsh_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let mut host = linux(dir.path());
    host.devices.kvm = dir.path().join("no-kvm");
    let tools = on_linux(FakeProcesses::script())
        .expect("virsh --version", Reply::ok("10.0.0"))
        .expect(PLUGINS, listing(&[("vagrant-libvirt", "0.12.2")]))
        .build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, true).await;
    assert_eq!(status_of(&statuses, Provider::Libvirt).reason, Some(Unavailable::NoKvm));
}

#[tokio::test(start_paused = true)]
async fn libvirt_joined_since_logging_in_waits_for_the_next_login() {
    let dir = tempfile::tempdir().unwrap();
    let tools = FakeProcesses::script()
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .expect("virsh --version", Reply::ok("10.0.0"))
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("mkisofs -version", Reply::ok("mkisofs 2.01"))
        .expect(PLUGINS, listing(&[("vagrant-libvirt", "0.12.2"), ("vagrant-qemu", "0.6.3")]))
        // Asked once for libvirt and QEMU both.
        .expect_times("id -un", 1, Reply::ok("ada"))
        .expect_times("id -nG ada", 1, Reply::ok("ada sudo libvirt"))
        .expect_times("id -nG", 1, Reply::ok("ada sudo"))
        .build();
    let (cx, _) = context(tools, linux(dir.path()));
    let statuses = detect(&cx, true).await;
    let libvirt = status_of(&statuses, Provider::Libvirt);
    assert_eq!(libvirt.reason, Some(Unavailable::NewLogin { group: "libvirt".into() }));
    assert_eq!(libvirt.account_lacks, Some(AccountLacks::NewLogin));
    // QEMU opens /dev/kvm through the kvm group, which this account had at login.
    let qemu = status_of(&statuses, Provider::Qemu);
    assert_eq!((qemu.reason.clone(), qemu.account_lacks), (None, None));
}

#[tokio::test(start_paused = true)]
async fn without_vagrant_the_hypervisors_are_probed_and_each_needs_vagrant() {
    // Setup needs "VirtualBox is there, Vagrant isn't". No plugin list is asked.
    let tools = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::ok("7.1.4r165100"))
        .expect("virsh --version", Reply::missing())
        .build();
    let dir = tempfile::tempdir().unwrap();
    let (cx, _) = context(tools, linux(dir.path()));
    let statuses = detect(&cx, false).await;
    assert_eq!(
        providers(&statuses),
        [
            Provider::Virtualbox,
            Provider::VmwareDesktop,
            Provider::Libvirt,
            Provider::Qemu,
            Provider::VmwareEsxi
        ]
    );
    assert_every!(&statuses, |s: &&ProviderStatus| s.reason == Some(Unavailable::VagrantMissing)
        && s.needs.first() == Some(&Need::Vagrant));
    let virtualbox = status_of(&statuses, Provider::Virtualbox);
    assert_eq!((virtualbox.hypervisor, virtualbox.needs.as_slice()), (Some(true), [Need::Vagrant].as_slice()));
    assert_eq!(status_of(&statuses, Provider::Libvirt).needs, [Need::Vagrant, Need::Plugin]);
    assert_eq!(status_of(&statuses, Provider::Libvirt).hypervisor, Some(false));
}

/// Fusion or Workstation installed (`vmrun` answers with its usage), Vagrant installed, with
/// `plugins`: what checking VMware Desktop alone asks.
fn vmware_machine(plugins: &[(&str, &str)]) -> FakeProcesses {
    FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect(PLUGINS, listing(plugins))
        .expect("vmrun", Reply::fail(255, "usage: vmrun [AUTHENTICATION-FLAGS] COMMAND [PARAMETERS]"))
        .build()
}

#[tokio::test]
async fn a_vmware_machine_without_the_plugin_or_the_utility_needs_both_and_is_ready_once_both_are_in() {
    let dir = tempfile::tempdir().unwrap();
    let host = linux(dir.path());
    // Fusion installed, other plugins, neither of VMware's two parts.
    let (before, _) = context(vmware_machine(&[("vagrant-qemu", "0.8.0"), ("vagrant_utm", "0.2.0")]), host.clone());
    let before = status(&before, Provider::VmwareDesktop).await.unwrap();
    assert_eq!(before.hypervisor, Some(true));
    assert_eq!(before.needs, [Need::Plugin, Need::VmwareUtility]);
    assert_eq!(before.utility, Some(UtilityState::Missing));
    assert!(!before.available);

    // The plugin alone: still not ready, and what is left is said.
    let (plugin_only, _) = context(vmware_machine(&[("vagrant-vmware-desktop", "3.0.5")]), host.clone());
    let plugin_only = status(&plugin_only, Provider::VmwareDesktop).await.unwrap();
    assert_eq!(plugin_only.needs, [Need::VmwareUtility]);
    assert_eq!(plugin_only.reason, Some(Unavailable::VmwareUtilityMissing));

    // Installed, its service not answering.
    write(&host.devices.vmware_utility, "");
    let (stopped, _) = context(vmware_machine(&[("vagrant-vmware-desktop", "3.0.5")]), host.clone());
    assert_eq!(
        status(&stopped, Provider::VmwareDesktop).await.unwrap().reason,
        Some(Unavailable::VmwareUtilityStopped)
    );

    // Both in: ready, nothing left to install.
    let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut running = host;
    running.devices.vmware_utility_service = service.local_addr().unwrap();
    let (after, _) = context(vmware_machine(&[("vagrant-vmware-desktop", "3.0.5")]), running);
    let after = status(&after, Provider::VmwareDesktop).await.unwrap();
    assert!(after.needs.is_empty() && after.available, "{:?}", after.reason);
    assert_eq!(after.utility, Some(UtilityState::Running));
}

#[tokio::test(start_paused = true)]
async fn a_remote_server_is_checked_alone_with_nothing_installed_here() {
    let dir = tempfile::tempdir().unwrap();
    let tools = FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect(PLUGINS, listing(&[("vagrant-vmware-esxi", "2.5.5")]))
        .build();
    let (cx, _) = context(tools, linux(dir.path()));
    let esxi = status(&cx, Provider::VmwareEsxi).await.unwrap();
    assert!(esxi.remote && esxi.available, "{esxi:?}");
    assert_eq!((esxi.hypervisor, esxi.plugin.as_deref()), (None, Some("vagrant-vmware-esxi")));
    // Not offered here: nothing is probed for it.
    let (nothing, _) = context(FakeProcesses::script().expect("vagrant --version", Reply::missing()).build(), linux(dir.path()));
    assert_eq!(status(&nothing, Provider::Hyperv).await, None);
}

// macOS.

/// A Mac on `arch` whose devices are under `root`, with UTM's bundle when `utm`.
fn mac(arch: Arch, root: &Path, utm: bool) -> Host {
    let host = host(Os::Macos, arch, root);
    if utm {
        std::fs::create_dir_all(&host.devices.utm_app).unwrap();
    }
    host
}

/// `defaults read`'s call for UTM's version on `host`.
fn utm_version_call(host: &Host) -> String {
    let plist = host.devices.utm_app.join("Contents").join("Info.plist");
    format!("defaults read {} CFBundleShortVersionString", plist.display())
}

/// A Mac's hypervisor probes, each missing unless the script answers it already.
fn on_mac(script: Script, arch: Arch) -> Script {
    script
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("prlctl", Reply::missing())
        .allow(arch.qemu_system(), Reply::missing())
}

#[tokio::test(start_paused = true)]
async fn an_apple_silicon_mac_is_offered_utm_and_not_virtualbox() {
    let dir = tempfile::tempdir().unwrap();
    let host = mac(Arch::Aarch64, dir.path(), true);
    let tools = on_mac(FakeProcesses::script(), Arch::Aarch64)
        .expect(&utm_version_call(&host), Reply::ok("4.7.5\n"))
        .expect(PLUGINS, listing(&[("vagrant_utm", "0.2.1")]))
        .build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, true).await;
    assert_eq!(
        providers(&statuses),
        [
            Provider::VmwareDesktop,
            Provider::Parallels,
            Provider::Qemu,
            Provider::Utm,
            Provider::VmwareEsxi
        ]
    );
    assert_eq!(status_of(&statuses, Provider::Utm).reason, None);
    assert_eq!(
        status_of(&statuses, Provider::Parallels).reason,
        Some(Unavailable::NotInstalled {
            program: Some("prlctl".into())
        })
    );
}

#[tokio::test(start_paused = true)]
async fn utm_and_its_plugin_too_old_say_which_to_update() {
    let dir = tempfile::tempdir().unwrap();
    let host = mac(Arch::Aarch64, dir.path(), true);
    let utm = |version: &str, plugin: &str| {
        let tools = on_mac(FakeProcesses::script(), Arch::Aarch64)
            .expect(&utm_version_call(&host), Reply::ok(version))
            .expect(PLUGINS, listing(&[("vagrant_utm", plugin)]))
            .build();
        context(tools, host.clone())
    };
    let (old_plugin, _) = utm("4.7.5", "0.1.6");
    let status = status_of(&detect(&old_plugin, true).await, Provider::Utm).clone();
    assert_eq!(
        status.reason,
        Some(Unavailable::PluginTooOld {
            plugin: "vagrant_utm".into(),
            version: "0.1.6".into(),
            min: "0.2.0".into()
        })
    );
    assert!(
        !status.plugin_installed && status.needs == [Need::Plugin],
        "too old reads as not installed: {status:?}"
    );
    let (old_utm, _) = utm("4.5.1", "0.2.0");
    assert_eq!(
        status_of(&detect(&old_utm, true).await, Provider::Utm).reason,
        Some(Unavailable::HypervisorTooOld {
            name: "UTM".into(),
            version: "4.5.1".into(),
            min: "4.6.5".into()
        })
    );
}

#[tokio::test(start_paused = true)]
async fn an_intel_mac_is_offered_virtualbox_and_utm_only_once_installed() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_mac(FakeProcesses::script(), Arch::X86_64)
        .expect("VBoxManage --version", Reply::ok("7.1.4r165100"))
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, mac(Arch::X86_64, dir.path(), false));
    let statuses = detect(&cx, true).await;
    assert_eq!(
        providers(&statuses),
        [
            Provider::Virtualbox,
            Provider::VmwareDesktop,
            Provider::Parallels,
            Provider::Qemu,
            Provider::VmwareEsxi
        ]
    );
    // No kernel module list to read on a Mac: VirtualBox answering is VirtualBox ready.
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason, None);
    // A Mac builds a VM's seed with hdiutil: mkisofs is never asked for (the script would fail).
    let qemu = status_of(&statuses, Provider::Qemu);
    assert_eq!((qemu.hypervisor, qemu.needs.as_slice()), (Some(true), [Need::Plugin].as_slice()));
}

// Windows.

/// A Windows PC on `arch` whose devices are under `root`, with the Hyper-V feature on when
/// `hyperv`.
fn windows(arch: Arch, root: &Path, hyperv: bool) -> Host {
    let host = host(Os::Windows, arch, root);
    if hyperv {
        write(&host.devices.hyperv_service, "");
    }
    host
}

/// `whoami /groups`' CSV line for a group `sid`.
fn groups(sid: &str) -> Reply {
    Reply::ok(&format!("\"Label\",\"Label\",\"{sid}\","))
}

/// A member of Hyper-V Administrators.
const HYPERV_ADMINS: &str = "S-1-5-32-578";

/// A PC's probes besides Hyper-V's, each hypervisor missing.
fn on_windows(script: Script) -> Script {
    script.allow("VBoxManage", Reply::missing()).allow("vmrun", Reply::missing())
}

#[tokio::test(start_paused = true)]
async fn a_pc_with_hyperv_on_and_its_hypervisor_running_is_ready_and_offers_no_qemu_or_libvirt() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_windows(FakeProcesses::script())
        .expect("whoami /groups", groups(HYPERV_ADMINS))
        .expect("powershell", Reply::ok("True\r\n"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, tools) = context(tools, windows(Arch::X86_64, dir.path(), true));
    let statuses = detect(&cx, true).await;
    assert_eq!(
        providers(&statuses),
        [Provider::Virtualbox, Provider::VmwareDesktop, Provider::Hyperv, Provider::VmwareEsxi]
    );
    assert_eq!(status_of(&statuses, Provider::Hyperv).reason, None);
    let asked = tools.last("powershell").unwrap();
    assert_eq!(asked.args.last().map(String::as_str), Some(hyperv::HYPERVISOR_PRESENT));
}

#[tokio::test(start_paused = true)]
async fn on_windows_an_account_that_may_not_manage_hyperv_is_told_how_to() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_windows(FakeProcesses::script())
        .expect_times("whoami /groups", 1, groups("S-1-16-8192"))
        .allow("powershell", Reply::ok("True"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, windows(Arch::X86_64, dir.path(), true));
    let hyperv = status_of(&detect(&cx, true).await, Provider::Hyperv).clone();
    assert_eq!(hyperv.reason, Some(Unavailable::HypervNeedsAdmin));
    assert_eq!(hyperv.account_lacks, Some(AccountLacks::HypervAdmin));
}

/// Regression (#135): Hyper-V turned on with its hypervisor off (`hypervisorlaunchtype off`,
/// virtualization off in the firmware) read as ready, and `vagrant up` failed minutes in.
#[tokio::test(start_paused = true)]
async fn an_installed_hyperv_whose_hypervisor_isnt_running_isnt_ready() {
    let dir = tempfile::tempdir().unwrap();
    let hyperv_said = |answer: Reply| {
        let tools = on_windows(FakeProcesses::script())
            .expect("whoami /groups", groups(HYPERV_ADMINS))
            .expect("powershell", answer)
            .expect(PLUGINS, Reply::ok(""))
            .build();
        context(tools, windows(Arch::X86_64, dir.path(), true))
    };
    for (answer, reason) in [
        (Reply::ok("False\r\n"), Some(Unavailable::HypervisorStopped)),
        (Reply::ok("True"), None),
        // Unknown adds no reason.
        (Reply::fail(1, "Get-CimInstance : Access denied"), None),
        (Reply::missing(), None),
    ] {
        let (cx, _) = hyperv_said(answer);
        assert_eq!(status_of(&detect(&cx, true).await, Provider::Hyperv).reason, reason);
    }
}

#[tokio::test(start_paused = true)]
async fn whoami_and_powershell_are_asked_for_an_installed_hyperv_only() {
    let dir = tempfile::tempdir().unwrap();
    // Neither is scripted: asking either fails the test.
    let tools = on_windows(FakeProcesses::script()).expect(PLUGINS, Reply::ok("")).build();
    let (cx, _) = context(tools, windows(Arch::X86_64, dir.path(), false));
    let hyperv = status_of(&detect(&cx, true).await, Provider::Hyperv).clone();
    assert_eq!((hyperv.hypervisor, hyperv.reason), (Some(false), Some(Unavailable::HypervOff)));
}

#[tokio::test(start_paused = true)]
async fn windows_on_arm_offers_hyperv_and_neither_virtualbox_nor_vmware() {
    let dir = tempfile::tempdir().unwrap();
    let tools = FakeProcesses::script()
        .expect("whoami /groups", groups(HYPERV_ADMINS))
        .expect("powershell", Reply::ok("True"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, windows(Arch::Aarch64, dir.path(), true));
    let statuses = detect(&cx, true).await;
    assert_eq!(providers(&statuses), [Provider::Hyperv, Provider::VmwareEsxi]);
}
