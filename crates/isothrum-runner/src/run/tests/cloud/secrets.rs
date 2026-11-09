//! Secrets in a cloud run: the flag delivered into its machine over SSH where no store holds it,
//! values in a state file, outputs the state marks sensitive, and a secret carried into a guest step
//! (`SendEnv`): never on a command line, in an event or in the run's log.

use std::sync::atomic::Ordering;

use super::*;

/// A start's options with the run's flag.
fn with_flag(region: &str) -> RunOptions {
    let mut o = opts(region, "labs");
    o.secrets.insert("FLAG", SecretValue::new("flag{it's $HOME}"));
    o
}

#[tokio::test(start_paused = true)]
async fn a_flag_with_no_store_to_read_it_from_is_delivered_into_its_machine_over_ssh() {
    // DigitalOcean and Linode have no secret store: the runner writes the flag into the machine,
    // whose set-up waits for it.
    let a = account_of(SOLO_WEB_AWS, 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&with_flag("eu-west-3"), &events).await.unwrap();
    let delivered = a.cloud.delivered.lock().unwrap().clone();
    assert_eq!(delivered, [format!("FLAG {}\n", crate::encoding::base64(b"flag{it's $HOME}"))]);
    let call = a.fake.last("/var/lib/isoloom/secrets").unwrap();
    assert!(call.args.contains(&format!("admin@{PUBLIC}")), "the machine's address and login: {call:?}");
    assert!(call.line().contains("sudo sh -c"), "{call:?}");
    assert!(!call.line().contains("flag{"), "never on a command line");
    assert!(!call.stdin.as_ref().unwrap().exists(), "the input goes once delivered");
    let shown = serde_json::to_string(&events.events()).unwrap();
    assert!(
        !shown.contains("flag{") && !shown.contains(&crate::encoding::base64(b"flag{it's $HOME}")),
        "never in an event"
    );
    // Delivered after the start's steps, before the machine's ready file is read.
    let lines = a.fake.lines();
    let deliver = lines.iter().position(|l| l.contains("/var/lib/isoloom/secrets")).unwrap();
    assert!(deliver > lines.iter().position(|l| l.starts_with("terraform apply")).unwrap());
    assert!(deliver < lines.iter().position(|l| l.contains("head -n 1")).unwrap());
}

#[tokio::test(start_paused = true)]
async fn a_delivery_reaches_its_machine_at_a_sensitive_address_output() {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_AWS).unwrap();
    for o in m["outputs"].as_array_mut().unwrap() {
        o["sensitive"] = serde_json::json!(o["name"] == "address_web");
    }
    // A sensitive output is never bound for a template: the ready file is read at the `ip`.
    m["ready"][1]["host"] = serde_json::json!("${outputs.ip}");
    let a = account_of(&m.to_string(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&with_flag("eu-west-3"), &Recorder::new()).await.unwrap();
    let call = a.fake.last("/var/lib/isoloom/secrets").unwrap();
    assert!(call.args.contains(&format!("admin@{PUBLIC}")), "the address itself, not its mask: {call:?}");
}

#[tokio::test(start_paused = true)]
async fn a_machine_still_booting_gets_its_secrets_once_ssh_answers() {
    let a = account_of(SOLO_WEB_AWS, 0);
    a.cloud.unreachable.store(3, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&with_flag("eu-west-3"), &Recorder::new()).await.unwrap();
    assert_eq!(a.cloud.delivered.lock().unwrap().len(), 1);
    assert_eq!(a.fake.calls().iter().filter(|c| c.line().contains("/var/lib/isoloom/secrets")).count(), 4);
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_never_answers_fails_the_delivery_after_its_patience() {
    let a = account_of(SOLO_WEB_AWS, 0);
    a.cloud.unreachable.store(u32::MAX, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&with_flag("eu-west-3"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ToolFailed);
    assert!(err.to_string().contains("guest-web") && err.to_string().contains("Connection refused"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_delivery_waiting_for_a_step_goes_right_after_it() {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_AWS).unwrap();
    let flag = m["secrets"].as_array_mut().unwrap().iter_mut().find(|s| s["name"] == "FLAG").unwrap();
    flag["deliver"][0]["during_step"] = serde_json::json!("apply");
    let a = account_of(&m.to_string(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&with_flag("eu-west-3"), &Recorder::new()).await.unwrap();
    assert_eq!(a.cloud.delivered.lock().unwrap().len(), 1, "once");
}

/// The fixture with its flag in a Terraform variable file of the run's state folder.
fn flag_in_a_state_file() -> String {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_AWS).unwrap();
    let flag = m["secrets"].as_array_mut().unwrap().iter_mut().find(|s| s["name"] == "FLAG").unwrap();
    flag["deliver"] = serde_json::json!([{"via": "state_file", "path": "inputs.tfvars.json", "format": "tfvars_json", "key": "flag"}]);
    m["params"][0]["deliver"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"via": "state_file", "path": "inputs.tfvars.json", "format": "tfvars_json"}));
    m.to_string()
}

#[tokio::test(start_paused = true)]
async fn values_in_a_state_file_are_written_at_up_kept_for_the_destroy_and_gone_after_it() {
    let a = account_of(&flag_in_a_state_file(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&with_flag("eu-west-3"), &Recorder::new()).await.unwrap();
    let file = a.runner.store.run_dir(run.name()).unwrap().join("inputs.tfvars.json");
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written, serde_json::json!({"flag": "flag{it's $HOME}", "region": "eu-west-3"}));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }
    // A destroy reads what its apply had, whatever it is given now.
    let mut later = opts("us-east-1", "default");
    later.secrets.insert("FLAG", SecretValue::new("another"));
    run.down(&later, &Recorder::new()).await.unwrap();
    let seen: serde_json::Value = serde_json::from_str(a.cloud.destroyed_with.lock().unwrap().as_deref().unwrap()).unwrap();
    assert_eq!(seen, written);
    assert!(!file.exists(), "a successful down removes it");
}

/// The fixture with an output the module marks sensitive and the manifest doesn't (a run's own
/// module Isoloom can't read), named in the message.
fn with_a_state_secret() -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    m["outputs"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "db_password", "role": "info", "sensitive": false,
        "from": {"kind": "terraform_output", "name": "db_password"}
    }));
    m["environment"]["message"] = serde_json::json!("The database takes ${outputs.db_password}; the site is at ${outputs.address_web}.");
    m.to_string()
}

#[tokio::test(start_paused = true)]
async fn an_output_the_state_marks_sensitive_is_masked_and_never_in_the_message() {
    let a = account_of(&with_a_state_secret(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    let started = run.up(&opts("eu-west-3", "labs"), &events).await.unwrap();
    let secret = started.outputs.iter().find(|o| o.name == "db_password").unwrap();
    assert!(secret.sensitive, "the state's word wins over the manifest's");
    assert_eq!((secret.value.as_deref(), secret.revealed()), (Some("***"), Some("hunter2-db")));
    assert_eq!(
        started.message.as_deref(),
        Some("The database takes ${outputs.db_password}; the site is at 198.51.100.10.")
    );
    run.check(&RunOptions::new(), &events).await.unwrap();
    let shown = serde_json::to_string(&events.events()).unwrap();
    assert!(!shown.contains("hunter2-db"), "never in an event: {shown}");
    assert!(shown.contains("admin/***"), "a log line holding it is masked");
    let status = run.status().await.unwrap();
    let written = format!("{} {status:?}", serde_json::to_string(&status).unwrap());
    assert!(!written.contains("hunter2-db"), "never in the status's JSON or Debug: {written}");
    let read = status.outputs.iter().find(|o| o.name == "db_password").unwrap();
    assert!(read.sensitive);
    assert_eq!((read.value.as_deref(), read.revealed()), (Some("***"), Some("hunter2-db")));
    assert!(!status.message.unwrap_or_default().contains("hunter2-db"));
}

/// The fixture as cloud-docker's: its flag an environment variable of the guest step that starts
/// Compose, carried over SSH (`SendEnv`), never on the command line.
fn compose_over_ssh() -> String {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_AWS).unwrap();
    m["lifecycle"]["up"].as_array_mut().unwrap().push(serde_json::json!({
        "id": "compose-up", "action": "ssh.run",
        "with": {"host": "${outputs.address_web}", "user": "${outputs.ssh_user}", "command": "cd /opt/isoloom && sudo -E docker compose up -d"}
    }));
    let flag = m["secrets"].as_array_mut().unwrap().iter_mut().find(|s| s["name"] == "FLAG").unwrap();
    flag["deliver"] = serde_json::json!([{"via": "env", "var": "FLAG", "steps": ["compose-up"]}]);
    m.to_string()
}

/// Every event the run showed and logged, as text.
fn everything(a: &Account, run: &crate::Running<'_>, events: &Recorder) -> String {
    let log = std::fs::read_to_string(a.runner.store.run_dir(run.name()).unwrap().join("events.jsonl")).unwrap();
    format!("{}\n{log}", serde_json::to_string(&events.events()).unwrap())
}

#[tokio::test(start_paused = true)]
async fn a_secret_carried_into_a_guest_step_never_shows_in_an_event_or_the_log() {
    let a = account_of(&compose_over_ssh(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&with_flag("eu-west-3"), &events).await.unwrap();
    let call = a.fake.last("docker compose up").unwrap();
    assert!(call.args.windows(2).any(|w| w == ["-o", "SendEnv=FLAG"]), "{call:?}");
    assert_eq!(call.env("FLAG"), Some("flag{it's $HOME}"));
    assert!(!call.line().contains("flag{"), "never on the command line");
    let shown = everything(&a, &run, &events);
    assert!(shown.contains("starting with FLAG=***"), "its line, masked: {shown}");
    assert!(!shown.contains("flag{"), "{shown}");
}

#[tokio::test(start_paused = true)]
async fn a_guest_step_that_fails_saying_a_secret_is_reported_masked() {
    let a = account_of(&compose_over_ssh(), 0);
    a.cloud.compose_fails.store(true, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    let err = run.up(&with_flag("eu-west-3"), &events).await.unwrap_err();
    assert!(err.to_string().contains("compose-up") && err.to_string().contains("FLAG=***"), "{err}");
    let shown = everything(&a, &run, &events);
    assert!(!shown.contains("flag{"), "{shown}");
}
