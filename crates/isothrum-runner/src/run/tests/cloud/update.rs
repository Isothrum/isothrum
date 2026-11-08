//! Bringing a running cloud run up to date with a regenerated manifest (a machine added to the
//! lab): `up`'s steps again, with the params it was started with, and it stays ready.

use super::*;
use crate::state::RunState;

/// The fixture with one more machine and the step that reaches it, as Isoloom writes a lab with
/// a machine added.
fn with_extra_machine() -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    let mut extra = m["machines"][0].clone();
    extra["name"] = "extra".into();
    m["machines"].as_array_mut().unwrap().push(extra);
    m["lifecycle"]["up"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "deliver-extra",
        "action": "ssh.run",
        "with": { "host": "${outputs.ip}", "user": "admin", "command": "touch /var/lib/isoloom/extra-delivered" }
    }));
    m.to_string()
}

fn applies(a: &Account) -> Vec<Call> {
    a.fake.calls().into_iter().filter(|c| c.line().starts_with("terraform apply")).collect()
}

#[tokio::test(start_paused = true)]
async fn an_update_applies_the_regenerated_manifest_with_the_runs_params_and_stays_ready() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "labs"), &Recorder::new()).await.unwrap();
    // The embedder regenerated the manifest with a machine added, and opens the run on it.
    std::fs::write(&a.folder.manifest, with_extra_machine()).unwrap();
    let regenerated = a.runner.open(&a.folder.manifest, None).unwrap();
    let before = applies(&a).len();
    // Given another region now: the run's own (recorded) one is what the apply gets.
    let events = Recorder::new();
    let updated = regenerated.update(&opts("us-east-1", "labs"), &events).await.unwrap();
    assert_eq!(updated.record.state, RunState::Ready);
    let applied = applies(&a);
    assert_eq!(applied.len(), before + 1, "the module applied again");
    assert_eq!(applied.last().unwrap().env("TF_VAR_region"), Some("eu-west-3"));
    assert!(
        a.fake.calls().iter().any(|c| c.line().contains("touch /var/lib/isoloom/extra-delivered")),
        "the added machine's step ran"
    );
    assert!(
        events.events().iter().all(|e| e.op == crate::state::Op::Update),
        "{:?}",
        events.events().first()
    );
}

#[tokio::test(start_paused = true)]
async fn a_run_that_is_not_up_is_not_updated() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.update(&opts("eu-west-3", "labs"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotRunning, "{err}");
    assert!(applies(&a).is_empty(), "nothing applied");
}
