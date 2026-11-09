//! Sealing a cloud run (`lifecycle.seal`): a second apply with the seal's param at `true` once
//! the machines are set up; replayed by every later operation, forgotten by a successful down.

use std::sync::atomic::Ordering;

use super::*;
use crate::{ErrorCode, Occupied};

/// The fixture with a seal: param `sealed` (the runner's, `false` by default) to the module,
/// and the apply that seals.
fn sealing() -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    m["params"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "sealed", "description": "The run is sealed: its internet: false networks closed at the cloud's firewalls.",
        "default": "false", "values": ["true", "false"], "set_by": "runner",
        "deliver": [{"via": "tf_var", "var": "sealed"}]
    }));
    let apply = m["lifecycle"]["up"].as_array().unwrap().iter().find(|s| s["id"] == "apply").unwrap().clone();
    let mut seal = apply;
    seal["id"] = serde_json::json!("seal");
    m["lifecycle"]["seal"] = serde_json::json!({"param": "sealed", "steps": [seal]});
    m.to_string()
}

/// The `sealed` variable of each `terraform` call whose line starts with `what`, in order.
fn sealed_in(a: &Account, what: &str) -> Vec<String> {
    let calls = a.fake.calls();
    calls
        .iter()
        .filter(|c| c.line().starts_with(what))
        .map(|c| c.env("TF_VAR_sealed").unwrap_or("-").to_owned())
        .collect()
}

#[tokio::test(start_paused = true)]
async fn a_run_is_sealed_once_ready_then_reported_ready() {
    let a = account_of(&sealing(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let started = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    assert_eq!(sealed_in(&a, "terraform apply"), ["false", "true"], "set up open, then sealed");
    let lines = a.fake.lines();
    let seal = lines.iter().rposition(|l| l.starts_with("terraform apply")).unwrap();
    assert!(seal > lines.iter().position(|l| l.contains("head -n 1")).unwrap(), "after the ready file");
    assert_eq!(started.record.params.get("sealed").map(String::as_str), Some("true"));
    assert_eq!(started.record.state, RunState::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_failed_seal_leaves_the_run_not_ready_and_says_so() {
    let a = account_of(&sealing(), 0);
    a.cloud.seal_fails.store(true, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NotSealed);
    assert!(err.to_string().contains("UnauthorizedOperation"), "{err}");
    let record = a.runner.store.record(run.name()).unwrap().unwrap();
    assert_eq!(record.state, RunState::Failed, "never ready unsealed");
    assert_eq!(record.last.unwrap().code, Some(ErrorCode::NotSealed));
    assert_ne!(run.status().await.unwrap().state, RunState::Ready);
    // Its destroy still replays the seal it tried.
    run.down(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    assert_eq!(sealed_in(&a, "terraform destroy"), ["true"]);
}

#[tokio::test(start_paused = true)]
async fn every_later_operation_replays_the_seal_and_a_down_forgets_it() {
    let a = account_of(&sealing(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    // A later start over what exists removes it with the seal it has, then starts open again.
    let again = opts("us-east-1", "default").with_occupied(Occupied::Replace);
    run.up(&again, &Recorder::new()).await.unwrap();
    assert_eq!(sealed_in(&a, "terraform destroy"), ["true"], "the leftovers' destroy keeps the seal");
    let destroy = a.fake.last("terraform destroy").unwrap();
    assert_eq!(destroy.env("TF_VAR_region"), Some("eu-west-3"), "and goes where they were applied");
    assert_eq!(sealed_in(&a, "terraform apply"), ["false", "true", "false", "true"]);
    run.down(&opts("us-east-1", "default"), &Recorder::new()).await.unwrap();
    assert_eq!(sealed_in(&a, "terraform destroy"), ["true", "true"], "the down replays it");
    let record = a.runner.store.record(run.name()).unwrap().unwrap();
    assert!(!record.params.contains_key("sealed"), "forgotten after a successful down");
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    assert_eq!(sealed_in(&a, "terraform apply")[4], "false", "a fresh start begins unsealed");
}
