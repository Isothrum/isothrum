use isothrum_runner::fake::{FakeProcesses, Reply};
use isothrum_test_support::assert_every;

use super::*;

#[tokio::test(start_paused = true)]
async fn a_qemu_with_its_plugin_is_ready() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect(
            PLUGINS,
            Reply::ok("1,,plugin-name,vagrant-qemu\n1,vagrant-qemu,plugin-version,0.6.3%!(VAGRANT_COMMA) global"),
        )
        .build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &tools, true).await;
    assert_eq!(status_of(&statuses, Provider::Qemu).reason, None);
}

#[tokio::test(start_paused = true)]
async fn a_qemu_without_its_plugin_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &tools, true).await;
    assert_eq!(
        status_of(&statuses, Provider::Qemu).reason.as_deref(),
        Some("Vagrant plugin `vagrant-qemu` is not installed")
    );
}

#[tokio::test(start_paused = true)]
async fn an_arm_host_probes_its_own_qemu() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host {
        arch: Arch::Arm64,
        ..linux(&dir.path().join("kvm"))
    };
    let tools = FakeProcesses::script()
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("virsh", Reply::missing())
        .expect("qemu-system-aarch64 --version", Reply::missing())
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on(&host, &tools, true).await;
    assert_eq!(
        status_of(&statuses, Provider::Qemu).reason.as_deref(),
        Some("`qemu-system-aarch64` was not found")
    );
}

#[tokio::test(start_paused = true)]
async fn a_vagrant_that_cant_list_its_plugins_is_one_reason_not_each_plugin_missing() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_linux(FakeProcesses::script())
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect("VBoxManage --version", Reply::ok("7.1.4"))
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect(PLUGINS, Reply::fail(1, "Vagrant failed to initialize at a very early stage"))
        .build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &tools, true).await;
    assert_eq!(
        status_of(&statuses, Provider::Qemu).reason.as_deref(),
        Some("Vagrant couldn't say which plugins it has (`vagrant plugin list`: Vagrant failed to initialize at a very early stage (exited with code 1))")
    );
    // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason, None);
}

#[tokio::test(start_paused = true)]
async fn a_virtualbox_that_isnt_installed_is_detected_as_such() {
    let dir = tempfile::tempdir().unwrap();
    let tools = on_linux(FakeProcesses::script())
        .expect("VBoxManage --version", Reply::missing())
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &tools, true).await;
    assert_eq!(status_of(&statuses, Provider::Virtualbox).reason.as_deref(), Some("`VBoxManage` was not found"));
}

#[tokio::test(start_paused = true)]
async fn libvirt_on_a_host_without_kvm_says_so_once_virsh_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host {
        kvm: dir.path().join("no-kvm"),
        ..linux(&dir.path().join("kvm"))
    };
    let tools = on_linux(FakeProcesses::script())
        .expect("virsh --version", Reply::ok("10.0.0"))
        .expect(PLUGINS, Reply::ok("1,,plugin-name,vagrant-libvirt"))
        .build();
    let statuses = detect_on(&host, &tools, true).await;
    let reason = status_of(&statuses, Provider::Libvirt).reason.clone().unwrap();
    assert!(reason.starts_with("KVM isn't available on this machine"), "{reason}");
}

#[tokio::test(start_paused = true)]
async fn without_vagrant_every_provider_says_so_and_nothing_is_probed() {
    let dir = tempfile::tempdir().unwrap();
    // No call is scripted: any probe fails the test.
    let tools = FakeProcesses::script().build();
    let statuses = detect_on(&linux(&dir.path().join("kvm")), &tools, false).await;
    let providers: Vec<Provider> = statuses.iter().map(|s| s.provider).collect();
    assert_eq!(
        providers,
        [
            Provider::Virtualbox,
            Provider::VmwareDesktop,
            Provider::Libvirt,
            Provider::Qemu,
            Provider::VmwareEsxi
        ]
    );
    assert_every!(&statuses, |s: &&ProviderStatus| s.reason.as_deref() == Some(NO_VAGRANT));
}

#[tokio::test(start_paused = true)]
async fn a_virtualbox_whose_service_never_answers_is_wedged() {
    assert!(vbox_wedged(&Stuck).await);
    let answering = FakeProcesses::expect(&[("VBoxManage list runningvms", Reply::ok(""))]);
    assert!(!vbox_wedged(&answering).await);
}

#[tokio::test(start_paused = true)]
async fn utm_is_asked_its_version_from_its_bundle() {
    let plist = "defaults read /Applications/UTM.app/Contents/Info.plist CFBundleShortVersionString";
    let tools = FakeProcesses::expect(&[(plist, Reply::ok("4.7.5\n"))]);
    assert_eq!(hypervisor_version(Provider::Utm, &tools).await.as_deref(), Some("4.7.5"));
    let silent = FakeProcesses::expect(&[(plist, Reply::ok(""))]);
    assert_eq!(hypervisor_version(Provider::Utm, &silent).await, None, "no version said, none read");
}

#[tokio::test(start_paused = true)]
async fn no_hypervisor_but_utm_is_asked_its_version() {
    let nothing = FakeProcesses::script().build();
    for provider in all().into_iter().filter(|p| *p != Provider::Utm) {
        assert_eq!(hypervisor_version(provider, &nothing).await, None, "{provider:?}");
    }
}

/// `whoami /groups`' CSV line for a group `sid`.
fn groups(sid: &str) -> Reply {
    Reply::ok(&format!("\"Label\",\"Label\",\"{sid}\","))
}

#[tokio::test(start_paused = true)]
async fn an_installed_hyperv_needs_admin_when_whoami_says_the_account_may_not_manage_it() {
    let not_allowed = FakeProcesses::script().expect_times("whoami /groups", 1, groups("S-1-16-8192")).build();
    assert!(needs_admin(Provider::Hyperv, Some(true), &not_allowed).await);
    let allowed = FakeProcesses::expect(&[("whoami /groups", groups("S-1-5-32-578"))]);
    assert!(!needs_admin(Provider::Hyperv, Some(true), &allowed).await);
    let unknown = FakeProcesses::expect(&[("whoami", Reply::missing())]);
    assert!(!needs_admin(Provider::Hyperv, Some(true), &unknown).await, "no answer adds no reason");
}

#[tokio::test(start_paused = true)]
async fn whoami_is_asked_for_an_installed_hyperv_only() {
    // Nothing is scripted: asking whoami fails the test.
    let nothing = FakeProcesses::script().build();
    assert!(!needs_admin(Provider::Hyperv, Some(false), &nothing).await, "not installed says so instead");
    assert!(!needs_admin(Provider::Hyperv, None, &nothing).await);
    assert!(!needs_admin(Provider::Virtualbox, Some(true), &nothing).await);
}

#[tokio::test(start_paused = true)]
async fn on_windows_an_account_that_may_not_manage_hyperv_is_told_how_to() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host {
        os: HostOs::Windows,
        hyperv_enabled: true,
        ..linux(&dir.path().join("kvm"))
    };
    let tools = FakeProcesses::script()
        .expect_times("whoami /groups", 1, groups("S-1-16-8192"))
        .allow("powershell", Reply::ok("True"))
        .expect(PLUGINS, Reply::ok(""))
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("qemu-system-x86_64", Reply::missing())
        .build();
    let statuses = detect_on(&host, &tools, true).await;
    assert_eq!(status_of(&statuses, Provider::Hyperv).reason.as_deref(), Some(HYPERV_NEEDS_ADMIN));
    assert!(statuses.iter().all(|s| s.provider != Provider::Libvirt), "libvirt is Linux's");
}
