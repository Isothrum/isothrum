//! Reaching a cloud machine: a guest step waits for its machine to answer over SSH, and only a
//! failed `terraform apply` is applied again.

use std::sync::atomic::Ordering;

use super::*;

/// The fixture, whose start copies the project into its machine over SSH right after the apply
/// (`deliver-web`, its last step), waiting `wait` seconds for the machine to answer.
fn copying(wait: &str) -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    let copy = m["lifecycle"]["up"].as_array_mut().unwrap().last_mut().unwrap();
    assert_eq!(copy["id"], "deliver-web", "the copy is the start's last step");
    copy["with"]["wait_s"] = wait.into();
    m.to_string()
}

/// The calls of the copy step.
fn copies(a: &Account) -> usize {
    a.fake.calls().iter().filter(|c| c.line().contains("tar -xzf")).count()
}

#[tokio::test(start_paused = true)]
async fn a_guest_step_waits_for_its_machine_to_answer_over_ssh() {
    let a = account_of(&copying("600"), 0);
    a.cloud.copy_unreachable.store(4, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts("eu-west-3", "demo"), &events).await.unwrap();
    assert_eq!(copies(&a), 5, "tried until it answered");
    let shown = serde_json::to_string(&events.events()).unwrap();
    assert!(!shown.contains("Connection refused"), "ssh's own complaints aren't the step's log");
    assert_eq!(shown.matches("doesn't answer over SSH yet").count(), 1, "said once");
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_answers_fails_the_step_naming_where_it_tried() {
    let a = account_of(&copying("60"), 0);
    a.cloud.copy_unreachable.store(u32::MAX, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unreachable);
    let text = err.to_string();
    assert!(
        text.contains("admin@198.51.100.10") && text.contains("60s") && text.contains("Connection refused"),
        "{text}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_command_that_ran_and_failed_is_never_tried_again() {
    let a = account_of(&copying("600"), 0);
    a.cloud.copy_fails.store(true, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ToolFailed);
    assert_eq!(copies(&a), 1);
}

/// A start of `manifest` refused as malformed, saying `why`, with nothing copied.
async fn refused_as_malformed(manifest: &str, why: &str) {
    let a = account_of(manifest, 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
    assert!(err.to_string().contains(why), "{err}");
    assert_eq!(copies(&a), 0);
}

#[tokio::test(start_paused = true)]
async fn a_wait_over_half_an_hour_or_not_in_seconds_is_a_malformed_manifest() {
    for (wait, why) in [
        ("1801", "wait_s: 1801 is over the 1800 seconds"),
        ("soon", "wait_s: `soon` isn't a number of seconds"),
    ] {
        refused_as_malformed(&copying(wait), why).await;
    }
}

#[tokio::test(start_paused = true)]
async fn an_ssh_step_without_its_host_or_with_a_login_ssh_cant_take_is_a_malformed_manifest() {
    for (host, why) in [
        ("", "an `ssh.run` needs its `host` and `command`"),
        ("web -oProxyCommand=x", "the SSH login `admin@web -oProxyCommand=x`"),
    ] {
        let mut m: serde_json::Value = serde_json::from_str(&copying("60")).unwrap();
        m["lifecycle"]["up"].as_array_mut().unwrap().last_mut().unwrap()["with"]["host"] = host.into();
        refused_as_malformed(&m.to_string(), why).await;
    }
}

#[tokio::test(start_paused = true)]
async fn only_a_failed_apply_is_applied_again() {
    // `apply_retries` is the module's: a guest step that failed fails the start, no re-apply.
    let a = account_of(&copying("600"), 0);
    a.cloud.copy_fails.store(true, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(a.fake.calls().iter().filter(|c| c.line().starts_with("terraform apply")).count(), 1);
}
