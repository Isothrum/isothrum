//! The controller: halted after `up` (and when that fails), waited for while it halts, booted
//! again for a check.

use super::*;

#[tokio::test]
async fn a_controller_powering_itself_off_after_up_is_waited_for_not_warned_about() {
    // Seen on libvirt: the halt after `up` races the controller's own poweroff and fails over SSH.
    let f = starts_without_a_tolerated_step(|e| e.halt_fails = Some("shutoff")).await;
    assert_eq!(f.state(CONTROLLER), "shutoff");
}

#[tokio::test]
async fn on_esxi_a_controller_already_off_is_halted_without_an_error_in_the_log() {
    // Seen on an ESXi host: the controller powers itself off at the end of its play, and ESXi
    // refuses the guest shutdown `vagrant halt` asks for.
    let f = fixture(HostOs::Linux);
    {
        let mut e = f.engine();
        e.plugins.push("vagrant-vmware-esxi".into());
        e.esxi_off_before_halt = true;
    }
    let mut connection = Secrets::new();
    connection.insert("ESXI_PASSWORD", SecretValue::new("pw"));
    let run = f.runner.open(&f.manifest, None).unwrap().with_connection(connection);
    let events = Recorder::new();
    run.up(
        &opts()
            .with_provider(isoloom_manifest::Provider::VmwareEsxi)
            .with_param("ESXI_HOSTNAME", "172.16.1.20"),
        &events,
    )
    .await
    .unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("InvalidPowerState") && !said.contains("step_tolerated"), "{said}");
    assert!(said.contains("isoloom-controller was already off"), "{said}");
    assert!(
        !f.fake.lines().iter().any(|l| l.starts_with("vagrant status isoloom-controller")),
        "no wait for a controller ESXi says is off: {:?}",
        f.fake.lines()
    );
}

/// Regression: the controller's halt took the step's `timeout_s`, never its limit on the run's
/// provider, so a slow ESXi halt was cut short at the generic limit.
#[tokio::test(start_paused = true)]
async fn the_controllers_halt_takes_the_steps_limit_on_the_runs_provider() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    for step in m["lifecycle"]["up"].as_array_mut().unwrap() {
        if step["id"] == "park-controller" {
            step["timeout_s"] = 10.into();
            step["provider_timeout_s"] = serde_json::json!({"vmware_esxi": 600});
        }
    }
    let f = fixture_from(HostOs::Linux, &m.to_string());
    {
        let mut e = f.engine();
        e.plugins.push("vagrant-vmware-esxi".into());
        e.halt_delay = std::time::Duration::from_secs(60);
    }
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&on_esxi(esxi_secrets()), &events).await.unwrap();
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("step_tolerated"), "{said}");
    assert_eq!(f.state(CONTROLLER), "poweroff");
}

#[tokio::test]
async fn a_controller_halts_with_its_lines_in_the_log() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    assert_eq!(f.state(CONTROLLER), "poweroff");
    let halted = events
        .events()
        .iter()
        .any(|e| matches!(&e.kind, crate::EventKind::Log { line, .. } if line == "==> isoloom-controller: Attempting graceful shutdown of VM..."));
    assert!(halted, "{:?}", events.events());
}

/// The start's events when `vagrant halt <controller>` fails and leaves it in `state`.
async fn up_with_a_halt_that_leaves(state: &'static str) -> String {
    let f = fixture(crate::runner::this_host());
    f.engine().halt_fails = Some(state);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    serde_json::to_string(&events.events()).unwrap()
}

/// Whether `said`, a start's events, warn that the controller's halt failed.
fn halt_failed(said: &str) -> bool {
    said.contains("step_tolerated") && said.contains("`vagrant halt isoloom-controller` failed: SSH connection was refused!")
}

#[tokio::test(start_paused = true)]
async fn a_controller_no_halt_stops_is_a_warning_after_up() {
    let said = up_with_a_halt_that_leaves("running").await;
    assert!(halt_failed(&said), "{said}");
}

/// Regression: the controller's halt was waited out only while it read `running`, so a UTM
/// controller (UTM's running state is `started`) still up after a failed halt counted as halted.
/// A state no provider lists (one on its way down) is waited out too.
#[tokio::test(start_paused = true)]
async fn a_controller_still_started_on_utm_or_on_its_way_down_is_no_halt() {
    for state in ["started", "shutting_down"] {
        let said = up_with_a_halt_that_leaves(state).await;
        assert!(halt_failed(&said), "{state}: {said}");
    }
}

/// The calls of a check run on a started run whose controller still reads as running, after
/// `set` (how it answers the halting probe): the checks must pass.
async fn checks_with_a_running_controller(set: impl FnOnce(&mut Engine)) -> Vec<String> {
    let (checked, calls, _) = check_with_a_running_controller(set).await;
    assert!(checked.unwrap().ok);
    calls
}

/// A check run on a started run whose controller still reads as running, after `set`: how it
/// ended (its error with its events' text), the calls, and how long it took.
async fn check_with_a_running_controller(set: impl FnOnce(&mut Engine)) -> (std::result::Result<crate::CheckReport, String>, Vec<String>, std::time::Duration) {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    {
        let mut e = f.engine();
        e.states.insert(CONTROLLER.into(), "running".into());
        set(&mut e);
    }
    let started = tokio::time::Instant::now();
    let events = Recorder::new();
    let checked = run.check(&opts(), &events).await;
    let took = started.elapsed();
    let checked = match checked {
        Ok(report) if report.ok => Ok(report),
        other => Err(format!("{other:?} {}", serde_json::to_string(&events.events()).unwrap())),
    };
    (checked, f.fake.lines(), took)
}

/// Whether `calls` boot the controller (`vagrant up` without its provisioners).
fn boots_controller(calls: &[String]) -> Option<usize> {
    calls.iter().rposition(|l| l == "vagrant up isoloom-controller --no-provision")
}

/// Asserts `calls` asked the controller whether it halts `times` times, and booted it only after.
fn asked_then_booted(calls: &[String], times: usize) {
    let probes = calls.iter().filter(|l| l.contains("test -e /run/isoloom-halting")).count();
    assert_eq!(probes, times, "{calls:#?}");
    let last_probe = calls.iter().rposition(|l| l.contains("isoloom-halting")).unwrap();
    assert!(boots_controller(calls).is_some_and(|boot| last_probe < boot), "{calls:#?}");
}

#[tokio::test(start_paused = true)]
async fn a_controller_still_halting_is_waited_for_before_it_boots() {
    let calls = checks_with_a_running_controller(|e| e.halting = 3).await;
    asked_then_booted(&calls, 3);
}

/// Regression: a UTM controller (UTM's running state is `started`) read as off, so one still
/// powering itself off was booted at once, racing its own poweroff.
#[tokio::test(start_paused = true)]
async fn a_controller_still_halting_on_utm_is_waited_for_before_it_boots() {
    let calls = checks_with_a_running_controller(|e| {
        e.states.insert(CONTROLLER.into(), "started".into());
        e.halting = 3;
    })
    .await;
    asked_then_booted(&calls, 3);
}

/// The race a vm run's checks lost: the controller's poweroff had begun, its SSH already closed,
/// while it still read as running. A failed probe is no answer: it is waited for, then booted.
#[tokio::test(start_paused = true)]
async fn a_controller_no_longer_answering_on_its_way_down_is_waited_for_then_booted() {
    let calls = checks_with_a_running_controller(|e| e.going_down = 2).await;
    assert!(boots_controller(&calls).is_some(), "booted once it was off: {calls:#?}");
}

/// A controller that reads as running and never answers over SSH (each ask hanging until its
/// time is up) is given up on within the wake's patience, with a clear error, not asked for
/// hours.
#[tokio::test(start_paused = true)]
async fn a_controller_running_but_never_answering_is_given_up_on_in_time() {
    let (checked, calls, took) = check_with_a_running_controller(|e| {
        e.going_down = u32::MAX;
        e.probe_delay = std::time::Duration::from_secs(100);
    })
    .await;
    assert!(took <= std::time::Duration::from_secs(200), "gave up after {took:?}");
    let said = checked.unwrap_err();
    assert!(said.contains("still reads as running after 180 s"), "{said}");
    assert!(boots_controller(&calls).is_none(), "not booted");
}

/// A controller left running (it answers it isn't halting) is used as it is: no boot.
#[tokio::test(start_paused = true)]
async fn a_controller_that_stays_up_is_not_booted_again() {
    let calls = checks_with_a_running_controller(|_| {}).await;
    assert!(boots_controller(&calls).is_none(), "{calls:#?}");
}

/// A VM on an ESXi host opens in the host's own web console (its Host Client), by the VM's id
/// Vagrant records.
#[tokio::test(start_paused = true)]
async fn a_vm_on_esxi_opens_in_its_host_clients_console() {
    let f = with_esxi_plugin();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&on_esxi(esxi_secrets()), &Recorder::new()).await.unwrap();
    let native = run.native_console("web").await.unwrap();
    assert_eq!(
        (native.app.as_str(), native.ways),
        (
            "ESXi",
            vec![crate::NativeOpen::Url {
                url: "https://172.16.1.20/ui/#/console/0000".into()
            }]
        )
    );
}
