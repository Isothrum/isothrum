//! Detection on Windows.

use super::*;

/// A Windows PC on `arch` whose devices are under `root`, with the Hyper-V feature on when
/// `hyperv`.
fn windows(arch: Arch, root: &Path, hyperv: bool) -> Host {
    let host = host(HostOs::Windows, arch, root);
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
    let (cx, tools) = context(tools, windows(Arch::Amd64, dir.path(), true));
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
    let (cx, _) = context(tools, windows(Arch::Amd64, dir.path(), true));
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
        context(tools, windows(Arch::Amd64, dir.path(), true))
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
    let (cx, _) = context(tools, windows(Arch::Amd64, dir.path(), false));
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
    let (cx, _) = context(tools, windows(Arch::Arm64, dir.path(), true));
    let statuses = detect(&cx, true).await;
    assert_eq!(providers(&statuses), [Provider::Hyperv, Provider::VmwareEsxi]);
}
