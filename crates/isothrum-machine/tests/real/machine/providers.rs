//! Each hypervisor's detection, checked against the hypervisor as it is here (its tool answers, or
//! its app is there), and its readiness for a start where the crate asks one (UTM answering,
//! QEMU's emulators).

use std::time::Duration;

use isothrum_machine::host::{os_name, qemu_system};
use isothrum_machine::providers::{self, Provider, Unavailable, ready};
use isothrum_machine::{Arch, Context, HostOs};
use isothrum_runner::{ProcessSpec, capture};

use super::{quit_utm, utm_running};
use crate::{answer, here};

/// How long UTM may take to open and answer.
const UTM_WAIT: Duration = Duration::from_secs(90);

/// The tool whose answer says `provider`'s hypervisor is installed here, with the arguments that
/// ask it its version.
fn tool(provider: Provider, cx: &Context) -> (String, Vec<&'static str>) {
    match provider {
        Provider::Virtualbox => ("VBoxManage".into(), vec!["--version"]),
        Provider::VmwareDesktop => ("vmrun".into(), vec![]),
        Provider::Parallels => ("prlctl".into(), vec!["--version"]),
        Provider::Libvirt => ("virsh".into(), vec!["--version"]),
        Provider::Qemu => (qemu_system(cx.host.arch).into(), vec!["--version"]),
        Provider::Hyperv => ("powershell".into(), vec!["-NoProfile", "-Command", "Get-Service vmms -ErrorAction Stop"]),
        _ => (String::new(), vec![]),
    }
}

/// Whether `provider`'s hypervisor is installed here, asked of the hypervisor itself: its tool
/// answers from `PATH` or from its app, or (UTM) its app is there. `vmrun` without arguments
/// prints its usage and exits 255: that it starts is enough.
async fn installed(cx: &Context, provider: Provider) -> bool {
    if provider == Provider::Utm {
        return cx.host.devices.utm_app.is_dir();
    }
    let (program, args) = tool(provider, cx);
    if runs(cx, &program, &args).await {
        return true;
    }
    for app in cx.host.devices.app_tool(&program) {
        if app.is_file() && runs(cx, &app.display().to_string(), &args).await {
            return true;
        }
    }
    false
}

/// Whether `program args` runs here: exits 0, or (`vmrun`, which has no version to ask) prints
/// its usage.
async fn runs(cx: &Context, program: &str, args: &[&str]) -> bool {
    let ran = capture(cx.runner.as_ref(), &ProcessSpec::new(program, args.iter().copied())).await;
    ran.is_ok_and(|c| c.success() || c.stdout.iter().chain(&c.stderr_tail).any(|l| l.contains("vmrun")))
}

/// Whether `reason` says the hypervisor itself is there, something beside it missing.
fn beside_the_hypervisor(reason: Option<&Unavailable>) -> bool {
    matches!(reason, Some(Unavailable::MkisofsMissing | Unavailable::VirtiofsdMissing { .. }))
}

/// `provider`'s status reads its hypervisor installed exactly when it is; skips where it isn't
/// offered, or isn't installed (once the status agrees).
async fn detected_as_it_is(provider: Provider) {
    let cx = here().await;
    let Some(status) = providers::status(&cx, provider).await else {
        skip!("{} isn't offered on {}", provider.id(), os_name(cx.host.os))
    };
    println!("{status:#?}");
    let there = installed(&cx, provider).await;
    let read_there = status.hypervisor == Some(true) || beside_the_hypervisor(status.reason.as_ref());
    assert_eq!(
        read_there,
        there,
        "detection reads {}'s hypervisor installed: {read_there}; it is: {there}",
        provider.id()
    );
    assert_eq!(status.available, status.reason.is_none(), "a status unavailable says why: {status:?}");
    if !there {
        skip!("{} isn't installed here", provider.id())
    }
    if !status.available {
        println!(
            "installed, not available: {}",
            status.reason.as_ref().map(Unavailable::message).unwrap_or_default()
        );
    }
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn virtualbox_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Virtualbox).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn vmware_desktop_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::VmwareDesktop).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn parallels_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Parallels).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn libvirt_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Libvirt).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn qemu_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Qemu).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn utm_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Utm).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn hyperv_is_detected_as_it_is_here() {
    detected_as_it_is(Provider::Hyperv).await;
}

/// UTM, closed or open, is ready for a start: opened hidden when it was closed, it answers within
/// the wait; and it is quit again when the test opened it.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn utm_is_ready_for_a_start() {
    let cx = here().await;
    if cx.host.os != HostOs::Macos {
        skip!("UTM is macOS's")
    }
    if !cx.host.devices.utm_app.is_dir() {
        skip!("UTM isn't installed here ({} isn't there)", cx.host.devices.utm_app.display())
    }
    let was_running = utm_running(&cx).await;
    let mut waited = false;
    let ready = ready::utm(&cx, UTM_WAIT, &mut || waited = true).await;
    if !was_running {
        quit_utm(&cx).await;
    }
    println!("UTM was running: {was_running}; waited for it: {waited}");
    if let Err(ready::NotReady::Needs(need)) = &ready {
        skip!("macOS refuses this session Apple events to UTM ({need:?}): its Automation permission")
    }
    assert_eq!(ready, Ok(()));
}

/// The emulators a start asks for are found exactly when QEMU's system emulator for that CPU
/// answers here.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn qemus_emulators_are_found_as_they_are_here() {
    let cx = here().await;
    let other = if cx.host.arch == Arch::Arm64 { Arch::Amd64 } else { Arch::Arm64 };
    let program = qemu_system(other);
    let there = answer(&cx, program, &["--version"]).await.is_some();
    let found = ready::emulators(&cx, [cx.host.arch, other]).await;
    println!("{program} answers: {there}; found: {found:?}");
    match found {
        Ok(emulated) => assert!(there && emulated == [other], "{emulated:?}"),
        Err(ready::NotReady::Emulator { arch, .. }) => {
            assert!(!there && arch == other);
            skip!("QEMU's emulator {program} isn't installed here")
        }
        Err(why) => panic!("{why}"),
    }
}
