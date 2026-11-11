//! Bringing a running cloud run up to date with a regenerated manifest (a machine added to the
//! run): `up`'s steps again, with the params it was started with, and it stays ready.

use super::*;
use crate::state::RunState;

/// The fixture with one more machine and the step that reaches it, as Isoloom writes a run with
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
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    // The embedder regenerated the manifest with a machine added, and opens the run on it.
    std::fs::write(&a.folder.manifest, with_extra_machine()).unwrap();
    let regenerated = a.runner.open(&a.folder.manifest, None).unwrap();
    let before = applies(&a).len();
    // Given another region now: the run's own (recorded) one is what the apply gets.
    let events = Recorder::new();
    let updated = regenerated.update(&opts("us-east-1", "demo"), &events).await.unwrap();
    assert_eq!(updated.record.state, RunState::Ready);
    let applied = applies(&a);
    assert_eq!(applied.len(), before + 1, "the module applied again");
    assert_eq!(applied.last().unwrap().env("TF_VAR_region"), Some("eu-west-3"));
    assert!(
        a.fake.calls().iter().any(|c| c.line().contains("touch /var/lib/isoloom/extra-delivered")),
        "the added machine's step ran"
    );
    assert!(
        events.events().iter().all(|e| e.op == Some(crate::state::Op::Update)),
        "{:?}",
        events.events().first()
    );
}

#[tokio::test(start_paused = true)]
async fn a_run_that_is_not_up_is_not_updated() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.update(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotRunning, "{err}");
    assert!(applies(&a).is_empty(), "nothing applied");
}

/// A console into any machine of the run: `ssh -t` to its address as its user, a login shell.
#[tokio::test(start_paused = true)]
async fn a_console_reaches_any_machine_of_the_run_over_ssh() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    let console = run.console_command("web", None).unwrap();
    assert_eq!(console.program, "ssh");
    assert_eq!(console.args.first().map(String::as_str), Some("-t"));
    assert!(console.args.iter().any(|x| x == "admin@198.51.100.10"), "{:?}", console.args);
    // In ssh's own folder, the run's state, which its config is named relative to: the console
    // the embedder runs reads the config there.
    assert_eq!(console.cwd, a.runner.store.run_dir(run.name()).unwrap());
    assert!(console.args.windows(2).any(|w| w == ["-F", "ssh_config"]), "{:?}", console.args);
    // Bash where the machine has it, with nothing of the shell redirected (its prompt is on its
    // error output).
    assert_eq!(
        console.args.last().map(String::as_str),
        Some("if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi")
    );
    let unknown = run.console_command("nope", None).unwrap_err();
    assert_eq!(unknown.code(), crate::ErrorCode::MachineUnknown, "{unknown}");
}

/// A console as an account: the same `ssh -t` as the module's user, then a `sudo -i` login as
/// the account (the module's user's shell when it is that account).
#[tokio::test(start_paused = true)]
async fn a_console_over_ssh_opens_as_the_account_asked() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    let console = run.console_command("web", Some("user")).unwrap();
    assert_eq!(console.args.first().map(String::as_str), Some("-t"));
    assert!(
        console.args.iter().any(|x| x == "admin@198.51.100.10"),
        "logs in as the module's user: {:?}",
        console.args
    );
    assert_eq!(console.args.last(), Some(&crate::driver::shells::login_shell_as("user")));
}

/// A machine that was running keeps what it was given: an update (which carries no launch's
/// values) delivers nothing to it again.
#[tokio::test(start_paused = true)]
async fn an_update_never_delivers_again_to_a_machine_already_set_up() {
    let a = account_of(SOLO_WEB_AWS, 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let mut secrets = crate::Secrets::new();
    secrets.insert("DEMO_SECRET", crate::SecretValue::new("token-real"));
    run.up(&opts("eu-west-3", "demo").with_secrets(secrets), &Recorder::new()).await.unwrap();
    assert_eq!(a.cloud.delivered.lock().unwrap().len(), 1, "delivered at up");
    run.update(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    assert_eq!(a.cloud.delivered.lock().unwrap().len(), 1, "web keeps its secret: nothing delivered again");
    // Regression: a machine running but unhealthy (a tainted Terraform resource) was taken for
    // one new to the run, and the update's default overwrote the launch's value.
    let state = a.runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate");
    let tainted = std::fs::read_to_string(&state)
        .unwrap()
        .replace(r#"{"instances":[{}]}"#, r#"{"instances":[{"status":"tainted"}]}"#);
    std::fs::write(&state, tainted).unwrap();
    run.update(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    assert_eq!(a.cloud.delivered.lock().unwrap().len(), 1, "an unhealthy web runs: it keeps its secret too");
}

/// A console into a machine the state gives no address (no `address_<machine>` output): not
/// running, rather than an SSH login to nowhere.
#[tokio::test(start_paused = true)]
async fn a_console_into_a_machine_with_no_address_is_not_running() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "demo"), &Recorder::new()).await.unwrap();
    let state = a.runner.store.run_dir(run.name()).unwrap().join("terraform.tfstate");
    std::fs::write(&state, r#"{"outputs": {}, "resources": []}"#).unwrap();
    let err = run.console_command("web", None).unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotRunning, "{err}");
}
