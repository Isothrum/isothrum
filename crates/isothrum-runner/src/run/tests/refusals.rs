//! What a start or a park refuses before anything runs: a mode the manifest has no steps for, a
//! param the runner sets, a host port it can't pin.

use super::*;
use crate::Park;

/// A fresh run with no free host port left for an ephemeral publish fails the pinning step,
/// naming the port, rather than start on an ephemeral one.
#[tokio::test]
async fn no_free_host_port_fails_the_pinning_of_a_fresh_run() {
    let f = fixture_with_ports(WEB_PORTAL, []);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(
        matches!(&err, crate::Error::ToolFailed { step, reason } if step == "pin-ports" && reason.contains("no free host port over tcp")),
        "{err}"
    );
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// Regression: a loopback that couldn't be bound (no file descriptor left) read as no free port
/// left, a diagnosis that sends the user looking for what holds their ports.
#[tokio::test]
async fn a_loopback_that_cant_be_bound_fails_the_pinning_with_why() {
    let f = fixture();
    f.ports.refuse_binds(std::io::ErrorKind::QuotaExceeded);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
    assert!(err.to_string().starts_with("a tcp port on 127.0.0.1 couldn't be bound: "), "{err}");
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}

/// A park in a mode the manifest has no steps for is refused (`not_parkable`) before anything
/// runs.
#[tokio::test]
async fn a_park_in_a_mode_the_manifest_lacks_is_not_parkable() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    m["lifecycle"]["park"].as_object_mut().unwrap().remove("shutdown");
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let calls = f.fake.calls().len();
    let err = run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotParkable, "{err}");
    assert_eq!(f.fake.calls().len(), calls, "nothing ran");
    run.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.unwrap();
}

/// A param the runner sets (`set_by: runner`) given by the embedder is refused before anything
/// runs, naming it.
#[tokio::test]
async fn a_param_the_runner_sets_is_refused_from_the_embedder() {
    let mut m: serde_json::Value = serde_json::from_str(WEB_PORTAL).unwrap();
    let param = m["params"].as_array_mut().unwrap().iter_mut().find(|p| p["name"] == "ISOLOOM_DERIVED").unwrap();
    param["set_by"] = "runner".into();
    let f = fixture_with(&m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts().with_param("ISOLOOM_DERIVED", "1"), &Recorder::new()).await.unwrap_err();
    assert!(
        err.to_string().contains("ISOLOOM_DERIVED") && err.to_string().contains("is set by the runner, not the embedder"),
        "{err}"
    );
    assert_eq!(f.called(" up --detach "), 0, "nothing started");
}
