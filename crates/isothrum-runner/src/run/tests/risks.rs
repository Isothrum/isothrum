//! The runtime risks, each pinned by a test that fails on the risky behaviour.

use super::*;
use crate::error::Error;
use crate::events::Emitter;
use crate::run::operation::Done;
use crate::state::{Op, RunState};
use crate::{ErrorCode, Park, WarningCode};

#[tokio::test]
async fn a_run_whose_lock_cant_be_taken_never_starts() {
    // A run lock that fails open lets the operation run unlocked.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    std::fs::create_dir_all(f.runner.store.lock_path(run.name())).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts(), &events).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Io);
    assert!(f.fake.calls().is_empty(), "no tool ran: {:?}", f.fake.lines());
    assert!(matches!(kinds(&events).as_slice(), [EventKind::Error { code: ErrorCode::Io, .. }]));
}

#[tokio::test]
async fn a_down_replays_the_params_the_run_was_started_with() {
    // A destroy used the profile's current settings (region, credentials), not the applied ones.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let mut now = RunOptions::default();
    now.params.insert("ISOLOOM_PUBLISH_ADDRESS".into(), "0.0.0.0".into());
    run.down(&now, &Recorder::new()).await.unwrap();
    let down = f.fake.last(" down --volumes").unwrap();
    assert_eq!(down.env("ISOLOOM_PUBLISH_ADDRESS"), Some("127.0.0.1"));
}

#[tokio::test]
async fn a_pinned_port_another_run_is_picking_is_never_handed_out_twice() {
    // Ports were picked, then released before Compose bound them, so two starts could pin
    // the same one. The OS here hands 41873 out twice; the runner's reservation refuses the second.
    let f = fixture();
    let other = crate::RunName::new("other-run").unwrap();
    assert!(
        f.runner.reserve(&other, 41873, isoloom_manifest::Protocol::Tcp),
        "another run starting meanwhile"
    );
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41874/"));
}

#[tokio::test]
async fn a_port_held_on_the_host_is_refused_before_anything_starts() {
    let f = fixture();
    f.ports.take(8080, isoloom_manifest::Protocol::Tcp);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let mut fixed = opts();
    fixed.params.insert("ISOLOOM_PUBLISH_FIXED".into(), "1".into());
    let err = run.up(&fixed, &Recorder::new()).await.unwrap_err();
    assert_eq!(err.to_string(), "host port 8080/tcp is in use");
    assert_eq!(f.called(" up --detach "), 0);
}

#[tokio::test]
async fn a_run_recorded_elsewhere_is_refused() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let mut here = opts();
    here.host_label = Some("pve-run".into());
    run.up(&here, &Recorder::new()).await.unwrap();
    here.host_label = Some("other-server".into());
    assert_eq!(run.down(&here, &Recorder::new()).await.unwrap_err().code(), ErrorCode::RunningElsewhere);
    let park = run.park(Park::Shutdown, &here, &Recorder::new()).await;
    assert_eq!(park.unwrap_err().code(), ErrorCode::RunningElsewhere);
    assert_eq!(f.called(" stop"), 0, "nothing parked elsewhere");
}

#[tokio::test]
async fn an_op_enters_the_flight_map_only_under_the_run_lock() {
    // `operate` marked the op in flight (and its guard freed the op's port reservations on
    // drop) BEFORE taking the per-run lock, so two overlapping ops on one run clobbered each
    // other's in-flight entry and freed each other's reservations. Entering now happens under the
    // lock, so an op still waiting for the lock is not yet in flight and its guard cannot outlive it.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let name = run.name().clone();
    // Hold the run's lock, as another operation on the same run would (whose `operate` created the
    // run's state folder before it locked).
    let lock_path = f.runner.store.lock_path(&name);
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let held = f.runner.locks.acquire(&name, &lock_path).await.unwrap();
    let o = opts();
    let events = Recorder::new();
    let mut up = std::pin::pin!(run.up(&o, &events));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), up.as_mut()).await.is_err(),
        "up must wait for the held run lock, not run",
    );
    assert!(
        f.runner.in_flight().is_empty(),
        "an op still waiting for the lock has not marked itself in flight: {:?}",
        f.runner.in_flight(),
    );
    drop(held);
    up.await.unwrap();
    assert!(f.runner.in_flight().is_empty(), "the op cleared itself once it finished");
}

#[tokio::test]
async fn a_failed_operation_whose_record_cant_be_updated_ends_with_its_own_error() {
    // The record's I/O error replaced the work's, and the closing state move was never said.
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let events = Recorder::new();
    let record = f.runner.store.run_dir(run.name()).unwrap().join("record.json");
    let err = run
        .operate(Op::Down, &opts(), &events, async |_: &RunOptions, _: &Emitter<'_>| -> crate::Result<Done<()>> {
            // The record can't be read back, nor written: a folder where it was.
            std::fs::remove_file(&record).unwrap();
            std::fs::create_dir(&record).unwrap();
            Err(Error::NotRunning { run: "web-portal".into() })
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotRunning);
    let kinds = kinds(&events);
    assert!(
        kinds.iter().any(|k| matches!(
            k,
            EventKind::Warning {
                code: WarningCode::RecordNotSaved,
                ..
            }
        )),
        "{kinds:?}"
    );
    assert!(
        kinds.contains(&EventKind::State {
            from: RunState::Stopping,
            to: RunState::Ready
        }),
        "{kinds:?}"
    );
    assert!(
        matches!(
            kinds.last(),
            Some(EventKind::Error {
                code: ErrorCode::NotRunning,
                ..
            })
        ),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn an_output_whose_format_cant_be_filled_is_said_at_up() {
    // It became an output with no value, and nothing said why.
    let f = fixture_with(&crate::testing::unfillable_format());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let started = run.up(&opts(), &events).await.unwrap();
    assert_eq!(started.outputs[0].value, None);
    let warned: Vec<String> = kinds(&events)
        .into_iter()
        .filter_map(|k| match k {
            EventKind::Warning {
                code: WarningCode::OutputUnformatted,
                message,
            } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        warned,
        ["an output has no value: outputs.web__portal.format: `${outputs.token}` has no value in this operation"]
    );
}

/// Regression: a network whose native name couldn't be filled (from an output the run hasn't
/// given yet) was shown without one, and nothing
/// said why.
#[tokio::test]
async fn a_network_native_name_that_cant_be_filled_is_traced() {
    let unfillable = WEB_PORTAL.replace(r#""native": "${run.name}_lab""#, r#""native": "${outputs.web__portal}_lab""#);
    assert_ne!(unfillable, WEB_PORTAL, "the fixture's network has a native name");
    let f = fixture_with(&unfillable);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let (status, traced) = crate::testing::traced(run.status()).await;
    assert_eq!(status.unwrap().networks[0].native, None);
    assert!(
        traced.contains("WARN") && traced.contains("a network's native name couldn't be filled"),
        "{traced}"
    );
    assert!(traced.contains("outputs.web__portal"), "with the render error: {traced}");
}

/// A step's failure says why as a driver's own call's does: with nothing on its standard error,
/// its last output lines (a login refused on stdout).
#[tokio::test]
async fn a_failed_step_with_a_silent_stderr_says_its_last_output_lines() {
    let f = fixture();
    let mut refused = Reply::ok("Please login as the user \"admin\" rather than the user \"root\".");
    refused.code = 1;
    let runner = whose_up_answers(&f, refused);
    let run = runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "step `up` failed: Please login as the user \"admin\" rather than the user \"root\". (exited with code 1)"
    );
}
