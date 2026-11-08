//! Scripted runs of a cloud-vm manifest on AWS (solo-web): a fake Terraform that writes and
//! removes the run's state (failing its first apply when asked), a fake `ssh-keygen`, and a fake
//! `ssh` for the guest. No real tool or cloud is ever reached.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::{Cloud, HostOs};

use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{LabFolder, SOLO_WEB_AWS, TEST_TIME, no_ports, processes, runner_config};
use crate::{EventKind, Recorder, RunOptions, RunState, Runner, WarningCode};

/// The machine's public address the fake apply reports.
const PUBLIC: &str = "198.51.100.10";

/// A test's runner on the cloud fixture and its fake tools.
struct Account {
    folder: LabFolder,
    fake: Arc<FakeProcesses>,
    cloud: Arc<FakeCloud>,
    runner: Runner,
}

/// What the fake tools do: how many applies fail first, how many deliveries find the machine
/// unreachable first, and what each delivery's input held.
#[derive(Default)]
struct FakeCloud {
    failing_applies: AtomicU32,
    unreachable: AtomicU32,
    compose_fails: AtomicBool,
    /// How many tries of the project's delivery find SSH not answering yet.
    copy_unreachable: AtomicU32,
    /// The delivery's command runs and fails (ssh connected).
    copy_fails: AtomicBool,
    /// The apply that seals the run fails.
    seal_fails: AtomicBool,
    /// The module copy's `main.tf` as the destroy found it.
    destroyed_module: Mutex<Option<String>>,
    delivered: Mutex<Vec<String>>,
    /// The state file `inputs.tfvars.json` as the destroy found it.
    destroyed_with: Mutex<Option<String>>,
}

fn account(failing_applies: u32) -> Account {
    account_of(&without_guest_delivery(), failing_applies)
}

/// The fixture without its flag's guest delivery.
fn without_guest_delivery() -> String {
    let mut m: serde_json::Value = serde_json::from_str(SOLO_WEB_AWS).unwrap();
    m["secrets"].as_array_mut().unwrap().retain(|s| s["name"] != "FLAG");
    m.to_string()
}

/// A test's runner on `manifest`, its first `failing_applies` applies failing.
fn account_of(manifest: &str, failing_applies: u32) -> Account {
    let folder = LabFolder::new("cloud-vm/aws", manifest);
    std::fs::write(folder.target_dir().join("main.tf"), "").unwrap();
    let cloud = Arc::new(FakeCloud::default());
    cloud.failing_applies.store(failing_applies, Ordering::SeqCst);
    let fake = Arc::new(FakeProcesses::new({
        let (home, cloud) = (folder.home.path().to_path_buf(), Arc::clone(&cloud));
        move |call| answer(&home, &cloud, call)
    }));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), no_ports(), HostOs::Linux)).unwrap();
    Account { folder, fake, cloud, runner }
}

/// The run's state folder the fake Terraform writes in.
fn state_dir(call: &Call) -> PathBuf {
    call.env("TF_DATA_DIR").map(|d| Path::new(d).parent().unwrap().to_path_buf()).unwrap()
}

fn answer(home: &Path, cloud: &FakeCloud, call: &Call) -> Reply {
    let line = call.line();
    if call.program == "ssh-keygen" {
        return crate::testing::keygen(home);
    }
    if line.starts_with("terraform apply") && call.env("TF_VAR_sealed") == Some("true") && cloud.seal_fails.load(Ordering::SeqCst) {
        return Reply::fail(1, "Error: updating Security Group (sg-web): UnauthorizedOperation");
    }
    if line.starts_with("terraform apply") {
        if cloud.failing_applies.load(Ordering::SeqCst) > 0 {
            cloud.failing_applies.fetch_sub(1, Ordering::SeqCst);
            return Reply::fail(
                1,
                "Error: creating EC2 Instance: InvalidParameterValue: Value (isoloom-web) for parameter iamInstanceProfile.name is invalid",
            );
        }
        let state = format!(
            r#"{{"outputs":{{"ip":{{"value":"{PUBLIC}"}},"address_web":{{"value":"{PUBLIC}"}},"ssh_user":{{"value":"admin"}},"ready_file":{{"value":"/var/lib/isoloom/ready"}},"db_password":{{"value":"hunter2-db","sensitive":true}},"published":{{"value":{{"web/web":"{PUBLIC}:8080"}}}}}},"resources":[{{"instances":[{{}}]}}]}}"#
        );
        std::fs::write(state_dir(call).join("terraform.tfstate"), state).unwrap();
        return Reply::ok("Apply complete! Resources: 9 added, 0 changed, 0 destroyed.");
    }
    if line.starts_with("terraform destroy") {
        let inputs = std::fs::read_to_string(state_dir(call).join("inputs.tfvars.json")).ok();
        *cloud.destroyed_with.lock().unwrap_or_else(PoisonError::into_inner) = inputs;
        let module = std::fs::read_to_string(state_dir(call).join("module/main.tf")).ok();
        *cloud.destroyed_module.lock().unwrap_or_else(PoisonError::into_inner) = module;
        std::fs::write(state_dir(call).join("terraform.tfstate"), r#"{"outputs":{},"resources":[]}"#).unwrap();
        return Reply::ok("Destroy complete! Resources: 9 destroyed.");
    }
    if call.program == "ssh" {
        return guest(cloud, call, &line);
    }
    Reply::ok("")
}

/// What the fake `ssh` answers: its version, a delivery, the project's copy, Compose's start, a
/// check runner, a ready file.
fn guest(cloud: &FakeCloud, call: &Call, line: &str) -> Reply {
    if call.args == ["-V"] {
        return Reply::ok("OpenSSH_9.9p2");
    }
    if line.contains("/var/lib/isoloom/secrets") {
        if cloud.unreachable.load(Ordering::SeqCst) > 0 {
            cloud.unreachable.fetch_sub(1, Ordering::SeqCst);
            return Reply::fail(255, "ssh: connect to host 198.51.100.10 port 22: Connection refused");
        }
        let input = std::fs::read_to_string(call.stdin.as_ref().unwrap()).unwrap();
        cloud.delivered.lock().unwrap_or_else(PoisonError::into_inner).push(input);
        return Reply::ok("");
    }
    if line.contains("tar -xzf") {
        if cloud.copy_unreachable.load(Ordering::SeqCst) > 0 {
            cloud.copy_unreachable.fetch_sub(1, Ordering::SeqCst);
            return Reply::fail(255, "ssh: connect to host 198.51.100.10 port 22: Connection refused");
        }
        if cloud.copy_fails.load(Ordering::SeqCst) {
            return Reply {
                stdout: vec!["tar: unexpected end of file".into()],
                ..Reply::fail(255, "")
            };
        }
        return Reply::ok("");
    }
    if line.contains("docker compose up") {
        // Compose prints a value its environment gave it; a failing start says it again.
        let said = format!("web  | starting with FLAG={}", call.env("FLAG").unwrap_or_default());
        return if cloud.compose_fails.load(Ordering::SeqCst) {
            Reply::fail(1, &said)
        } else {
            Reply::ok(&said)
        };
    }
    if call.stdin.is_some() && line.ends_with("sh -s") {
        Reply::ok("signed in to the database as admin/hunter2-db\nisoloom-check: PASS checks/web-answers.sh\nisoloom-check: END 1 passed, 0 failed")
    } else if line.contains("head -n 1") {
        Reply::ok("ready\n--isoloom-log--\n")
    } else {
        Reply::ok("")
    }
}

/// A start's options: where and as whom, the account's keys.
fn opts(region: &str, profile: &str) -> RunOptions {
    let mut secrets = Secrets::new();
    secrets.insert("AWS_ACCESS_KEY_ID", SecretValue::new("AKIAFAKE"));
    secrets.insert("AWS_SECRET_ACCESS_KEY", SecretValue::new("s3cr3t-key"));
    RunOptions::new()
        .with_param("region", region)
        .with_param("AWS_PROFILE", profile)
        .with_param("allowed_cidr", "203.0.113.7/32")
        .with_secrets(secrets)
        .with_host_label("aws-1")
}

#[tokio::test(start_paused = true)]
async fn a_cloud_run_records_its_cloud_and_expiry_and_gives_terraform_its_place_and_keys() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let started = run.up(&opts("eu-west-3", "labs"), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.cloud, Some(Cloud::Aws));
    assert_eq!(started.record.expires_at, Some(TEST_TIME + 240 * 60), "the manifest's default life");
    let apply = a.fake.last("terraform apply").unwrap();
    assert_eq!(apply.env("TF_VAR_region"), Some("eu-west-3"));
    assert_eq!(apply.env("TF_VAR_expires_at"), Some((TEST_TIME + 240 * 60).to_string().as_str()));
    assert_eq!(apply.env("AWS_PROFILE"), Some("labs"));
    assert_eq!(apply.env("AWS_SECRET_ACCESS_KEY"), Some("s3cr3t-key"));
    assert!(!apply.line().contains("s3cr3t"), "never on a command line");
    let entry = started.outputs.iter().find(|o| o.name == "web_web").unwrap();
    assert_eq!(entry.value.as_deref(), Some("http://198.51.100.10:8080/"));
    assert_eq!(started.record.state, RunState::Ready);
}

#[tokio::test(start_paused = true)]
async fn a_lifetime_the_embedder_gives_sets_the_expiry() {
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let opts = opts("eu-west-3", "labs").with_lifetime(std::time::Duration::from_secs(3600));
    let started = run.up(&opts, &Recorder::new()).await.unwrap();
    assert_eq!(started.record.expires_at, Some(TEST_TIME + 3600));
}

#[tokio::test(start_paused = true)]
async fn a_destroy_goes_to_the_region_and_profile_applied_never_the_current_ones() {
    // R2: a destroy used the account's current region and profile, so it "succeeded" against an
    // empty region while the run kept billing where it was created.
    let a = account(0);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    run.up(&opts("eu-west-3", "labs"), &Recorder::new()).await.unwrap();
    run.down(&opts("us-east-1", "default"), &Recorder::new()).await.unwrap();
    let destroy = a.fake.last("terraform destroy").unwrap();
    assert_eq!(destroy.env("TF_VAR_region"), Some("eu-west-3"));
    assert_eq!(destroy.env("AWS_PROFILE"), Some("labs"));
    assert_eq!(destroy.env("TF_VAR_expires_at"), Some((TEST_TIME + 240 * 60).to_string().as_str()));
    assert_eq!(destroy.env("AWS_ACCESS_KEY_ID"), Some("AKIAFAKE"), "the connection is the current one");
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

#[tokio::test(start_paused = true)]
async fn an_apply_that_converges_on_a_second_try_is_applied_again() {
    // `recovery.apply_retries`: an IAM instance profile is eventually consistent; the first apply
    // can't use it yet, the second can.
    let a = account(1);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let events = Recorder::new();
    let started = run.up(&opts("eu-west-3", "labs"), &events).await.unwrap();
    assert_eq!(started.record.state, RunState::Ready);
    assert_eq!(a.fake.calls().iter().filter(|c| c.line().starts_with("terraform apply")).count(), 2);
    let retried = events
        .events()
        .into_iter()
        .any(|e| matches!(e.kind, EventKind::Warning { code, .. } if code == WarningCode::Retrying));
    assert!(retried, "a warning says the apply runs again");
}

#[tokio::test(start_paused = true)]
async fn an_apply_that_keeps_failing_fails_the_start_after_its_retries() {
    let a = account(2);
    let run = a.runner.open(&a.folder.manifest, None).unwrap();
    let err = run.up(&opts("eu-west-3", "labs"), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::ToolFailed);
    assert_eq!(
        a.fake.calls().iter().filter(|c| c.line().starts_with("terraform apply")).count(),
        2,
        "one retry"
    );
}

mod module;
mod reach;
mod seal;
mod secrets;
