//! Detection on macOS.

use super::*;

// macOS.

/// A Mac on `arch` whose devices are under `root`, with UTM's bundle when `utm`.
fn mac(arch: Arch, root: &Path, utm: bool) -> Host {
    let host = host(HostOs::Macos, arch, root);
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

/// A Mac's hypervisor probes, each missing unless the script answers it already; UTM's bundle,
/// where there is one, whole (no copy under way, its signature verifying).
fn on_mac(script: Script, arch: Arch) -> Script {
    script
        .allow("xattr", Reply::ok(""))
        .allow("codesign --verify --strict", Reply::ok(""))
        .allow("VBoxManage", Reply::missing())
        .allow("vmrun", Reply::missing())
        .allow("prlctl", Reply::missing())
        .allow(crate::host::qemu_system(arch), Reply::missing())
}

#[tokio::test(start_paused = true)]
async fn an_apple_silicon_mac_is_offered_utm_and_not_virtualbox() {
    let dir = tempfile::tempdir().unwrap();
    let host = mac(Arch::Arm64, dir.path(), true);
    let tools = on_mac(FakeProcesses::script(), Arch::Arm64)
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
    let host = mac(Arch::Arm64, dir.path(), true);
    let utm = |version: &str, plugin: &str| {
        let tools = on_mac(FakeProcesses::script(), Arch::Arm64)
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
    let tools = on_mac(FakeProcesses::script(), Arch::Amd64)
        .expect("VBoxManage --version", Reply::ok("7.1.4r165100"))
        .expect("VBoxManage list runningvms", Reply::ok(""))
        .expect("qemu-system-x86_64 --version", Reply::ok("QEMU 10.0"))
        .expect(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, mac(Arch::Amd64, dir.path(), false));
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

/// Regression (a Mac installed from scratch): a UTM bundle Finder was still copying (or had cut
/// short) read as UTM installed, and macOS said it was damaged when it was opened. It isn't
/// installed until it is whole.
#[tokio::test(start_paused = true)]
async fn a_utm_bundle_copied_only_in_part_is_not_utm_installed() {
    let dir = tempfile::tempdir().unwrap();
    let host = mac(Arch::Arm64, dir.path(), true);
    let tools = on_mac(FakeProcesses::script(), Arch::Arm64)
        .expect(
            &format!("xattr {}", host.devices.utm_app.display()),
            Reply::ok("com.apple.metadata:kMDItemResumableCopy"),
        )
        .allow(PLUGINS, Reply::ok(""))
        .build();
    let (cx, _) = context(tools, host);
    let statuses = detect(&cx, true).await;
    assert!(!providers(&statuses).contains(&Provider::Utm) || status_of(&statuses, Provider::Utm).hypervisor == Some(false));
}
