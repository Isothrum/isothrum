use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;
use WslState::*;

/// The classifier, on the signals of the Windows machines it was written for.
mod classify;

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
