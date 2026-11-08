//! The attack box on a lab run as one VM per machine in a cloud account: an instance of its own,
//! which the module makes while its `attack_box` variable names the image.

use std::sync::atomic::Ordering;

use super::*;
use crate::BoxPick;
use isoloom_manifest::Role;

/// The fixture as Isoloom now writes it: the module's `attack_box` variable as a param.
fn with_attack_box() -> String {
    let mut m: serde_json::Value = serde_json::from_str(&without_guest_delivery()).unwrap();
    m["params"].as_array_mut().unwrap().push(serde_json::json!({
        "name": "attack_box",
        "description": "The attack box's container image; empty: none.",
        "set_by": "embedder",
        "deliver": [{ "via": "tf_var", "var": "attack_box" }],
        "default": ""
    }));
    m.to_string()
}

fn applies(a: &Account) -> Vec<Call> {
    a.fake.calls().into_iter().filter(|c| c.line().starts_with("terraform apply")).collect()
}

/// Attach applies the module again with the image, waits for the box over SSH, and reports it on
/// the lab's network; status shows it; the shell is `ssh -t` then `docker exec`; detach applies
/// with the image cleared, and the box is gone.
#[tokio::test(start_paused = true)]
async fn the_box_is_an_instance_the_module_adds_and_removes() {
    let a = account_of(&with_attack_box(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let opts = opts("eu-west-3", "labs");
    run.up(&opts, &Recorder::new()).await.unwrap();
    let up_applies = applies(&a).len();
    assert_eq!(applies(&a)[0].env("TF_VAR_attack_box"), Some(""), "no box at up");

    let pick = BoxPick {
        image: Some("me/kali:2026"),
        ..BoxPick::default()
    };
    let report = run.attach_with(&opts, pick, &Recorder::new()).await.unwrap();
    assert_eq!(applies(&a).len(), up_applies + 1, "the module applied once more");
    assert_eq!(applies(&a).last().unwrap().env("AWS_PROFILE"), Some("labs"), "with the run's account");
    assert_eq!(a.cloud.attack_image.lock().unwrap().as_deref(), Some("me/kali:2026"));
    assert_eq!((report.machine.as_str(), report.address.as_deref()), ("isoloom-access", Some("10.80.0.77")));
    assert_eq!(report.network.as_deref(), Some("app"), "on the lab's network");
    let waited = a.fake.calls().into_iter().find(|c| c.line().contains("attack-box-ready")).unwrap();
    assert!(waited.args.iter().any(|x| x.contains("198.51.100.77")), "{:?}", waited.args);

    let status = run.status().await.unwrap();
    let access = status.machines.iter().find(|m| m.role == Role::Access).expect("the box in status");
    assert_eq!(access.name, "isoloom-access");

    let shell = run.shell_command(None).unwrap();
    assert_eq!(shell.args.first().map(String::as_str), Some("-t"));
    assert!(
        shell.args.last().unwrap().starts_with("sudo docker exec -it isoloom-access "),
        "{:?}",
        shell.args
    );

    run.detach_with(&opts, None, &Recorder::new()).await.unwrap();
    assert_eq!(applies(&a).last().unwrap().env("TF_VAR_attack_box"), Some(""), "applied with no box");
    let after = run.status().await.unwrap();
    assert!(after.machines.iter().all(|m| m.role != Role::Access), "the box is gone");
    let applied = applies(&a).len();
    run.detach_with(&opts, None, &Recorder::new()).await.unwrap();
    assert_eq!(applies(&a).len(), applied, "a second detach applies nothing");
}

/// A box whose image can't start fails the attach with Docker's reason.
#[tokio::test(start_paused = true)]
async fn a_box_that_cannot_start_says_why() {
    let a = account_of(&with_attack_box(), 0);
    a.cloud.attack_fails.store(true, Ordering::SeqCst);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let opts = opts("eu-west-3", "labs");
    run.up(&opts, &Recorder::new()).await.unwrap();
    let err = run.attach_with(&opts, BoxPick::default(), &Recorder::new()).await.unwrap_err();
    assert!(err.to_string().contains("manifest unknown"), "{err}");
    assert_eq!(
        a.cloud.attack_image.lock().unwrap().as_deref(),
        Some("kalilinux/kali-rolling"),
        "the default image"
    );
}

/// A cloud-vm module from before the box (no `attack_box` param) still refuses it.
#[tokio::test(start_paused = true)]
async fn a_module_without_the_box_refuses_it() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let opts = opts("eu-west-3", "labs");
    run.up(&opts, &Recorder::new()).await.unwrap();
    let err = run.attach_with(&opts, BoxPick::default(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::Unsupported, "{err}");
}

/// An instance the account refuses leaves the box's security group: the attach fails with the
/// cloud's reason, and a detach applies the module again to clear what was made.
#[tokio::test(start_paused = true)]
async fn a_detach_clears_what_a_refused_box_left() {
    let a = account_of(&with_attack_box(), 0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let opts = opts("eu-west-3", "labs");
    run.up(&opts, &Recorder::new()).await.unwrap();
    let pick = BoxPick {
        image: Some("refused"),
        ..BoxPick::default()
    };
    let err = run.attach_with(&opts, pick, &Recorder::new()).await.unwrap_err();
    assert!(err.to_string().contains("Free Tier"), "{err}");
    let before = applies(&a).len();
    run.detach_with(&opts, None, &Recorder::new()).await.unwrap();
    assert_eq!(applies(&a).len(), before + 1, "applied again with no box");
    assert_eq!(applies(&a).last().unwrap().env("TF_VAR_attack_box"), Some(""));
}
