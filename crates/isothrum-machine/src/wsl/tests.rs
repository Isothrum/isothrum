use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;
use WslState::*;

/// A machine where nothing could be read.
fn blank() -> Probes {
    Probes {
        status: Run::Failed,
        version: Run::Failed,
        lxss_service: None,
        wsl_service: None,
        vm_platform: None,
        wsl_feature: None,
        hypervisor: None,
        reboot_pending: None,
        renames_pending: None,
        installed_this_boot: false,
        restarted_for_wsl: false,
    }
}

/// Windows 11 without WSL: wsl.exe is there, and only offers to install.
fn win11_without_wsl() -> Probes {
    Probes {
        lxss_service: Some(false),
        wsl_service: Some(false),
        vm_platform: Some(Feature::Disabled),
        wsl_feature: Some(Feature::Disabled),
        hypervisor: Some(false),
        reboot_pending: Some(false),
        renames_pending: Some(false),
        ..blank()
    }
}

/// Windows 11 with the Store WSL and no distribution, after the restart.
fn win11_store_ready() -> Probes {
    Probes {
        status: Run::Ok,
        version: Run::Ok,
        wsl_service: Some(true),
        vm_platform: Some(Feature::Enabled),
        hypervisor: Some(true),
        ..win11_without_wsl()
    }
}

/// Windows 10 21H2 with the WSL feature (no Store WSL: `--version` is unknown to it).
fn win10_inbox_ready() -> Probes {
    Probes {
        status: Run::Ok,
        version: Run::Failed,
        lxss_service: Some(true),
        wsl_service: Some(false),
        vm_platform: Some(Feature::Enabled),
        wsl_feature: Some(Feature::Enabled),
        hypervisor: Some(true),
        reboot_pending: Some(false),
        renames_pending: Some(false),
        installed_this_boot: false,
        restarted_for_wsl: false,
    }
}

/// The Store WSL turned on, virtualization off in the firmware: the platform reads as on,
/// no hypervisor ever starts, and something re-adds file renames at every boot.
fn firmware_off() -> Probes {
    Probes {
        status: Run::Failed,
        hypervisor: Some(false),
        renames_pending: Some(true),
        ..win11_store_ready()
    }
}

#[test]
fn a_machine_without_wsl_is_missing() {
    assert_eq!(classify(&win11_without_wsl()), Missing);
    // Windows 10, whose wsl.exe may not exist at all.
    assert_eq!(
        classify(&Probes {
            status: Run::Missing,
            version: Run::Missing,
            ..win11_without_wsl()
        }),
        Missing
    );
    // The features couldn't be read: the services and wsl.exe still agree.
    assert_eq!(
        classify(&Probes {
            vm_platform: None,
            wsl_feature: None,
            hypervisor: None,
            ..win11_without_wsl()
        }),
        Missing
    );
    // A restart for an update, or renames, are no sign of WSL.
    assert_eq!(
        classify(&Probes {
            reboot_pending: Some(true),
            renames_pending: Some(true),
            ..win11_without_wsl()
        }),
        Missing
    );
}

#[test]
fn wsl_on_and_running_is_ready() {
    assert_eq!(classify(&win11_store_ready()), Ready);
    assert_eq!(classify(&win10_inbox_ready()), Ready);
    // Without a distribution `wsl --status` may fail: the platform running is enough.
    assert_eq!(
        classify(&Probes {
            status: Run::Failed,
            ..win11_store_ready()
        }),
        Ready
    );
    // A hung wsl.exe doesn't hide the rest.
    assert_eq!(
        classify(&Probes {
            status: Run::TimedOut,
            version: Run::TimedOut,
            ..win11_store_ready()
        }),
        Ready
    );
    // WMI unavailable: WSL answering, with nothing waiting for a restart, is enough.
    assert_eq!(
        classify(&Probes {
            vm_platform: None,
            wsl_feature: None,
            hypervisor: None,
            ..win11_store_ready()
        }),
        Ready
    );
    // An update waiting for a restart, while WSL answers.
    assert_eq!(
        classify(&Probes {
            reboot_pending: Some(true),
            renames_pending: Some(true),
            ..win11_store_ready()
        }),
        Ready
    );
    // Renames left by others never hold a working WSL back.
    assert_eq!(
        classify(&Probes {
            renames_pending: Some(true),
            ..win10_inbox_ready()
        }),
        Ready
    );
}

#[test]
fn a_pending_update_doesnt_hide_a_store_wsl_without_a_distribution() {
    // The usual state after `wsl --install --no-distribution` and its restart: `--status`
    // fails (no distribution), and Windows Update waits for a restart of its own.
    let store = Probes {
        status: Run::Failed,
        reboot_pending: Some(true),
        ..win11_store_ready()
    };
    assert_eq!(classify(&store), Ready);
    // Known by its service alone (`--version` timed out).
    assert_eq!(
        classify(&Probes {
            version: Run::TimedOut,
            ..store
        }),
        Ready
    );
    // The WSL feature (no Store WSL) gives no such proof: the restart may be its own.
    let inbox = Probes {
        status: Run::Failed,
        reboot_pending: Some(true),
        ..win10_inbox_ready()
    };
    assert_eq!(classify(&inbox), NeedsReboot);
}

#[test]
fn unreadable_servicing_is_no_proof_nothing_is_pending() {
    let unread = Probes {
        vm_platform: None,
        wsl_feature: None,
        hypervisor: None,
        reboot_pending: None,
        ..win11_store_ready()
    };
    assert_eq!(classify(&unread), Unknown);
    assert_eq!(
        classify(&Probes {
            reboot_pending: Some(false),
            ..unread
        }),
        Ready
    );
}

#[test]
fn turned_on_but_not_restarted_needs_a_reboot() {
    // Right after the caller's own install: the Store WSL is in, the platform reads as on, no
    // hypervisor yet.
    let after_install = Probes {
        status: Run::Failed,
        version: Run::Ok,
        wsl_service: Some(true),
        vm_platform: Some(Feature::Enabled),
        hypervisor: Some(false),
        reboot_pending: Some(true),
        installed_this_boot: true,
        ..win11_without_wsl()
    };
    assert_eq!(classify(&after_install), NeedsReboot);
    // Hyper-V already ran a hypervisor, and WSL answers: still the restart first.
    assert_eq!(
        classify(&Probes {
            hypervisor: Some(true),
            status: Run::Ok,
            ..after_install
        }),
        NeedsReboot
    );
    // Servicing says nothing, but the caller's install left renames.
    assert_eq!(
        classify(&Probes {
            hypervisor: Some(true),
            reboot_pending: Some(false),
            renames_pending: Some(true),
            ..after_install
        }),
        NeedsReboot
    );
    // The app was reopened before the restart, and WMI still reads the features as off.
    let reopened = Probes {
        vm_platform: Some(Feature::Disabled),
        wsl_feature: Some(Feature::Disabled),
        wsl_service: Some(false),
        version: Run::Failed,
        ..after_install
    };
    assert_eq!(classify(&reopened), NeedsReboot);
    // The caller's install ran this boot and the servicing state can't be read.
    assert_eq!(
        classify(&Probes {
            reboot_pending: None,
            ..reopened
        }),
        NeedsReboot
    );
    // Turned on by hand (`wsl --install` in PowerShell) on Windows 10: no mark of the caller's.
    let by_hand = Probes {
        status: Run::Failed,
        vm_platform: Some(Feature::Enabled),
        wsl_feature: Some(Feature::Enabled),
        lxss_service: Some(false),
        hypervisor: Some(false),
        reboot_pending: Some(true),
        ..win11_without_wsl()
    };
    assert_eq!(classify(&by_hand), NeedsReboot);
}

#[test]
fn our_install_that_changed_nothing_is_judged_on_the_rest() {
    // It ran this boot, yet no restart or rename is pending: WSL was ready already.
    assert_eq!(
        classify(&Probes {
            installed_this_boot: true,
            ..win11_store_ready()
        }),
        Ready
    );
}

#[test]
fn virtualization_off_in_the_firmware_never_loops_on_restarts() {
    // Renames re-added at every boot are no reason to restart (again).
    let off = firmware_off();
    assert_eq!(classify(&off), Unknown);
    assert!(virtualization_off(&off, classify(&off)));
    // After the caller's restart, even with servicing waiting for another one: no second ask.
    let after_ours = Probes {
        restarted_for_wsl: true,
        reboot_pending: Some(true),
        ..off
    };
    assert_eq!(classify(&after_ours), Unknown);
    assert!(virtualization_off(&after_ours, classify(&after_ours)));
    // The same pending restart without one of the caller's yet: that one is worth trying, once.
    let before_ours = Probes {
        reboot_pending: Some(true),
        ..off
    };
    assert_eq!(classify(&before_ours), NeedsReboot);
    assert!(!virtualization_off(&before_ours, NeedsReboot));
    // A new install of the caller's starts over.
    assert_eq!(
        classify(&Probes {
            installed_this_boot: true,
            ..after_ours
        }),
        NeedsReboot
    );
    // A working machine is never told virtualization is off.
    assert!(!virtualization_off(&win11_store_ready(), Ready));
}

#[test]
fn what_the_signals_cant_settle_is_unknown() {
    assert_eq!(classify(&blank()), Unknown);
    // Everything timed out.
    assert_eq!(
        classify(&Probes {
            status: Run::TimedOut,
            version: Run::TimedOut,
            ..blank()
        }),
        Unknown
    );
    // A hung wsl.exe with no other sign of WSL: maybe it's there and stuck.
    assert_eq!(
        classify(&Probes {
            status: Run::TimedOut,
            ..win11_without_wsl()
        }),
        Unknown
    );
    // On but no hypervisor and nothing pending: perhaps virtualization is off in the firmware.
    let no_hypervisor = Probes {
        hypervisor: Some(false),
        ..win10_inbox_ready()
    };
    assert_eq!(classify(&no_hypervisor), Unknown);
    assert!(virtualization_off(&no_hypervisor, Unknown));
    // Only the platform on, nothing of WSL: neither ready nor plainly missing.
    assert_eq!(
        classify(&Probes {
            vm_platform: Some(Feature::Enabled),
            ..win11_without_wsl()
        }),
        Unknown
    );
}

#[test]
fn wsl_1_without_the_platform_is_missing_wsl_2() {
    let wsl1 = Probes {
        vm_platform: Some(Feature::Disabled),
        hypervisor: Some(false),
        ..win10_inbox_ready()
    };
    assert_eq!(classify(&wsl1), Missing);
    // Unless the platform is being turned on.
    assert_eq!(
        classify(&Probes {
            reboot_pending: Some(true),
            ..wsl1
        }),
        NeedsReboot
    );
}

#[test]
fn the_wmi_output_is_read_by_key() {
    let out = "features=ok\r\nfeature:VirtualMachinePlatform=1\r\nfeature:Microsoft-Windows-Subsystem-Linux=2\r\nhypervisor=True\r\n";
    assert_eq!(parse_cim(out), (Some(Feature::Enabled), Some(Feature::Disabled), Some(true)));
    // An edition without the feature doesn't list it.
    assert_eq!(
        parse_cim("features=ok\nfeature:VirtualMachinePlatform=3\nhypervisor=False\n"),
        (Some(Feature::Absent), Some(Feature::Absent), Some(false))
    );
    // The feature query failed; the hypervisor one didn't.
    assert_eq!(parse_cim("hypervisor=False\n"), (None, None, Some(false)));
    // InstallState 4 (unknown), and nothing at all.
    assert_eq!(
        parse_cim("features=ok\nfeature:VirtualMachinePlatform=4\nfeature:Microsoft-Windows-Subsystem-Linux=1"),
        (None, Some(Feature::Enabled), None)
    );
    assert_eq!(parse_cim(""), (None, None, None));
}

#[test]
fn the_state_serializes_in_camel_case() {
    assert_eq!(serde_json::to_value(NeedsReboot).unwrap(), serde_json::json!("needsReboot"));
}

#[test]
fn a_script_is_encoded_as_base64_of_its_utf16() {
    // `[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes(...))`'s.
    assert_eq!(encoded("exit 0"), "ZQB4AGkAdAAgADAA");
    assert_eq!(encoded("a"), "YQA=");
    assert_eq!(encoded("ab"), "YQBiAA==");
    assert_eq!(encoded(""), "");
}

/// Windows with the Store WSL ready, each probe as `reg`, `wsl` and PowerShell answer it.
fn windows_ready() -> FakeProcesses {
    let cim = "features=ok\r\nfeature:VirtualMachinePlatform=1\r\nfeature:Microsoft-Windows-Subsystem-Linux=2\r\nhypervisor=True\r\n";
    FakeProcesses::model(move |call| {
        let arg = |i: usize| call.args.get(i).map_or("", String::as_str);
        match (call.program.as_str(), arg(0), arg(1)) {
            ("wsl", "--status", _) => Reply::ok("Default Version: 2"),
            ("wsl", "--version", _) => Reply::ok("WSL version: 2.3.26.0"),
            ("powershell", "-NoProfile", _) => Reply::ok(cim),
            ("reg", "query", key) if key.ends_with(r"Services\WslService") => Reply::ok("HKEY_LOCAL_MACHINE\\..."),
            ("reg", "query", _) => Reply::fail(1, "ERROR: The system was unable to find the specified registry key or value."),
            _ => Reply::missing(),
        }
    })
}

#[tokio::test(start_paused = true)]
async fn the_probes_read_wsl_the_registry_and_wmi_by_exit_code_and_key() {
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.os = HostOs::Windows;
    let tools = Arc::new(windows_ready());
    let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
    let cx = Context::new(runner, host);
    let found = check(&cx, Marks::default()).await.unwrap();
    let p = found.probes;
    assert_eq!((p.status, p.version), (Run::Ok, Run::Ok));
    assert_eq!(
        (p.lxss_service, p.wsl_service, p.reboot_pending, p.renames_pending),
        (Some(false), Some(true), Some(false), Some(false))
    );
    assert_eq!(
        (p.vm_platform, p.wsl_feature, p.hypervisor),
        (Some(Feature::Enabled), Some(Feature::Disabled), Some(true))
    );
    assert_eq!((found.state, found.virtualization_off), (Ready, false));
    let wsl = tools.made("wsl --status");
    assert_eq!(wsl[0].env("WSL_UTF8"), Some("1"));
    let queries: Vec<Vec<String>> = tools.made("reg query").into_iter().map(|c| c.args[1..].to_vec()).collect();
    assert_eq!(queries.len(), 4, "{queries:?}");
    let renames = [r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager", "/v", "PendingFileRenameOperations"];
    assert!(queries.iter().any(|q| q == &renames), "{queries:?}");
}

#[tokio::test(start_paused = true)]
async fn the_callers_marks_reach_the_classifier_and_only_windows_is_checked() {
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.os = HostOs::Windows;
    let cx = Context::new(Arc::new(windows_ready()), host);
    let marks = Marks {
        installed_this_boot: true,
        restarted_for_wsl: false,
    };
    let p = check(&cx, marks).await.unwrap().probes;
    assert!(p.installed_this_boot && !p.restarted_for_wsl);
    let linux = Context::new(Arc::new(FakeProcesses::script().build()), linux_host(std::path::Path::new("/nonexistent")));
    assert!(check(&linux, Marks::default()).await.is_none(), "nothing run off Windows");
}

#[tokio::test(start_paused = true)]
async fn a_probe_reads_as_ok_failed_timed_out_or_missing() {
    let fake = FakeProcesses::script()
        .expect("ok", Reply::ok("x"))
        .expect("failed", Reply::fail(1, ""))
        .expect("missing", Reply::missing())
        .build();
    let cx = Context::new(Arc::new(fake), linux_host(std::path::Path::new("/nonexistent")));
    let run = |program: &'static str| {
        let cx = &cx;
        async move { outcome(&cx.probe_within(WSL_TIMEOUT, &ProcessSpec::new(program, Vec::<String>::new())).await) }
    };
    assert_eq!(
        (run("ok").await, run("failed").await, run("missing").await),
        (Run::Ok, Run::Failed, Run::Missing)
    );
    let stuck = Context::new(Arc::new(crate::testing::Stuck), linux_host(std::path::Path::new("/nonexistent")));
    assert_eq!(
        outcome(&stuck.probe_within(WSL_TIMEOUT, &ProcessSpec::new("wsl", ["--status"])).await),
        Run::TimedOut
    );
    assert_eq!(registry(&stuck, &["HKLM"]).await, None, "a registry read that can't tell");
}
