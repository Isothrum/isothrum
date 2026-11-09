use std::path::Path;
use std::sync::Arc;

use isoloom_manifest::HostOs;

use super::*;
use crate::process::fake::{FakeProcesses, Reply};
use crate::testing::{LabFolder, SOLO_WEB_PROXMOX, WEB_PORTAL, no_ports, processes, runner_config};
use crate::{ErrorCode, RunName};

/// A runner beside a run holding `manifest` for `target`, no tool ever answering.
fn runner(target: &str, manifest: &str) -> (LabFolder, Runner) {
    let folder = LabFolder::new(target, manifest);
    let fake = Arc::new(FakeProcesses::new(|_| Reply::ok("")));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), no_ports(), HostOs::Linux)).unwrap();
    (folder, runner)
}

/// An old deployment's folder: its state, its backup and Terraform's data folder.
fn deployment(dir: &Path) -> std::path::PathBuf {
    let old = dir.join("deployments").join("solo-web").join("proxmox");
    std::fs::create_dir_all(old.join(".terraform")).unwrap();
    std::fs::write(old.join("terraform.tfstate"), r#"{"resources":[{"instances":[{}]}]}"#).unwrap();
    std::fs::write(old.join("terraform.tfstate.backup"), "{}").unwrap();
    std::fs::write(old.join(".terraform").join("terraform.tfstate"), r#"{"backend":{"config":{"path":"old"}}}"#).unwrap();
    old
}

fn adoption(old: &Path) -> Adoption {
    let params = [
        ("node", "pve-run"),
        ("region", "eu-west-3"),
        ("ssh_private_key_file", "/k"),
        ("ISOLOOM_DERIVED", "1"),
    ];
    Adoption::new(RunName::new("app-solo-web").unwrap(), old)
        .with_params(params.map(|(k, v)| (k.to_owned(), v.to_owned())))
        .with_host_label("pve-1")
        .with_expires_at(1_792_070_000)
        .with_started_at("2026-10-15T11:00:00Z")
}

#[tokio::test]
async fn a_deployment_taken_over_is_a_ready_run_holding_its_state() {
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    let record = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap();
    assert_eq!(record.state, RunState::Ready);
    assert_eq!((record.target, record.driver), (isoloom_manifest::Target::Proxmox, Driver::Terraform));
    assert_eq!(
        record.params.keys().collect::<Vec<_>>(),
        ["ISOLOOM_DERIVED", "node"],
        "only the manifest's embedder params"
    );
    assert_eq!(record.host_label.as_deref(), Some("pve-1"));
    assert_eq!(record.expires_at, Some(1_792_070_000));
    assert_eq!(record.started_at.as_deref(), Some("2026-10-15T11:00:00Z"));
    assert_eq!(runner.store.record(&record.name).unwrap(), Some(record.clone()));
    let state = runner.store.run_dir(&record.name).unwrap();
    assert!(state.join("terraform.tfstate").is_file() && state.join("terraform.tfstate.backup").is_file());
    assert!(
        !state.join(".terraform").exists(),
        "Terraform's data folder names the old backend: init makes a new one"
    );
    assert!(!old.join("terraform.tfstate").exists() && !old.join("terraform.tfstate.backup").exists());
    assert!(old.join(".terraform").is_dir(), "left where it was");
}

#[tokio::test]
async fn taking_the_same_deployment_over_twice_is_refused_and_changes_nothing() {
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    let first = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap();
    let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::AdoptRefused);
    assert!(err.to_string().contains("has a record already"), "{err}");
    assert_eq!(runner.store.record(&first.name).unwrap(), Some(first));
    // Under another name, the old folder holds no state any more.
    let again = Adoption::new(RunName::new("other").unwrap(), &old);
    let gone = runner.adopt(&folder.manifest, again).await.unwrap_err();
    assert!(gone.to_string().contains("holds no terraform.tfstate"), "{gone}");
    assert!(runner.store.record(&RunName::new("other").unwrap()).unwrap().is_none());
}

#[tokio::test]
async fn a_run_that_has_a_state_already_is_never_overwritten() {
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    let state = runner.store.run_dir(&RunName::new("app-solo-web").unwrap()).unwrap();
    std::fs::write(state.join("terraform.tfstate"), "mine").unwrap();
    let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
    assert!(err.to_string().contains("has a Terraform state already"), "{err}");
    assert_eq!(std::fs::read_to_string(state.join("terraform.tfstate")).unwrap(), "mine");
    assert!(old.join("terraform.tfstate").is_file(), "the old state stays where it was");
}

#[tokio::test]
async fn only_a_terraform_manifest_takes_a_deployment_over() {
    let (folder, runner) = runner("docker", WEB_PORTAL);
    let old = deployment(folder.home.path());
    let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::AdoptRefused);
    assert!(err.to_string().contains("driver is compose, not terraform"), "{err}");
    assert!(old.join("terraform.tfstate").is_file());
}

#[tokio::test]
async fn a_backup_alone_is_not_a_deployment() {
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    std::fs::remove_file(old.join("terraform.tfstate")).unwrap();
    let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
    assert!(err.to_string().contains("holds no terraform.tfstate"), "{err}");
    assert!(old.join("terraform.tfstate.backup").is_file());
}

#[tokio::test]
async fn a_deployment_with_no_start_time_is_recorded_as_started_when_taken_over() {
    // A recorded start is what makes `down` replay the params it was applied with.
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    let adoption = Adoption::new(RunName::new("bare").unwrap(), &old).with_slot(3);
    let record = runner.adopt(&folder.manifest, adoption).await.unwrap();
    assert_eq!(record.started_at.as_deref(), Some("2026-10-15T12:00:00Z"));
    assert_eq!(record.slot, Some(3), "the slot it was applied in");
}

/// A record of run `name` left as `state` by an earlier operation (a down: absent).
fn earlier(runner: &Runner, name: &str, state: RunState) -> RunRecord {
    let mut record = RunRecord::fresh(
        RunName::new(name).unwrap(),
        "/old/run/.isoloom/proxmox/run.json".into(),
        isoloom_manifest::Target::Proxmox,
        Driver::Terraform,
    );
    record.state = state;
    record.params.insert("node".into(), "old-node".into());
    record.slot = Some(7);
    runner.store.save(&record).unwrap();
    record
}

#[tokio::test]
async fn a_run_an_earlier_down_left_absent_takes_a_deployment_over() {
    // A run once run through the runner and stopped, then started on the embedder's own path: its
    // record says absent and its state went with the down, so that deployment is adopted.
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    earlier(&runner, "app-solo-web", RunState::Absent);
    let record = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap();
    assert_eq!(record.state, RunState::Ready);
    assert_eq!(
        record.params.get("node").map(String::as_str),
        Some("pve-run"),
        "the deployment's, not the earlier run's"
    );
    assert_eq!(record.manifest, std::path::absolute(&folder.manifest).unwrap());
    assert_eq!(runner.store.record(&record.name).unwrap(), Some(record.clone()));
    assert!(runner.store.run_dir(&record.name).unwrap().join("terraform.tfstate").is_file());
    assert!(!old.join("terraform.tfstate").exists());
}

#[tokio::test]
async fn a_run_that_isnt_absent_is_never_taken_over() {
    for state in [RunState::Ready, RunState::Failed, RunState::Parked, RunState::Starting, RunState::Stopping] {
        let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
        let old = deployment(folder.home.path());
        let before = earlier(&runner, "app-solo-web", state);
        let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
        assert_eq!(err.code(), ErrorCode::AdoptRefused, "{state:?}");
        assert!(err.to_string().contains("has a record already"), "{state:?}: {err}");
        assert_eq!(runner.store.record(&before.name).unwrap(), Some(before), "{state:?}: unchanged");
        assert!(old.join("terraform.tfstate").is_file(), "{state:?}: the old state stays");
    }
}

#[tokio::test]
async fn an_absent_run_that_still_has_a_state_is_never_overwritten() {
    let (folder, runner) = runner("proxmox", SOLO_WEB_PROXMOX);
    let old = deployment(folder.home.path());
    let before = earlier(&runner, "app-solo-web", RunState::Absent);
    let state = runner.store.run_dir(&before.name).unwrap().join("terraform.tfstate");
    std::fs::write(&state, "mine").unwrap();
    let err = runner.adopt(&folder.manifest, adoption(&old)).await.unwrap_err();
    assert!(err.to_string().contains("has a Terraform state already"), "{err}");
    assert_eq!(std::fs::read_to_string(&state).unwrap(), "mine");
    assert_eq!(runner.store.record(&before.name).unwrap(), Some(before));
}
