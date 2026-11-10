use super::*;
use crate::testing::proxmox_manifest;

fn state(text: &str) -> state::TfState {
    state::parse(text).unwrap()
}

#[test]
fn every_machine_is_absent_until_the_state_holds_resources() {
    let m = proxmox_manifest();
    let o = observed(&m, &state::TfState::default());
    assert!(o.machines.iter().all(|x| x.state == MachineState::Absent));
    assert!(o.published.is_empty() && o.outputs.is_empty());
}

#[test]
fn an_applied_state_runs_every_machine_and_publishes_at_the_routers_address() {
    let m = proxmox_manifest();
    let tf = state(r#"{"outputs":{"ip":{"value":"10.10.10.7"},"published":{"value":{"web/web":"10.10.10.7:8080"}}},"resources":[{"instances":[{}]}]}"#);
    let o = observed(&m, &tf);
    assert!(o.machines.iter().all(|x| x.state == MachineState::Running), "{:?}", o.machines);
    assert_eq!(o.published.get(&("web".to_owned(), "web".to_owned())), Some(&8080));
    assert_eq!(o.outputs.get("ip").map(String::as_str), Some("10.10.10.7"));
    let web = o.machines.iter().find(|x| x.name == "web").unwrap();
    assert_eq!(web.addresses.get("app").map(String::as_str), Some("10.80.0.10"));
    // Regression: a machine's own published ports were always empty on a Terraform run.
    assert_eq!(web.published.iter().map(|p| (p.port, p.host_port)).collect::<Vec<_>>(), [(80, 8080)]);
}

/// Regression: an entry of `published` that didn't read as `<machine>/<port>` at
/// `<address>:<port>` was dropped without a word.
#[tokio::test]
async fn a_published_entry_that_doesnt_read_is_left_out_and_traced() {
    let tf = state(r#"{"outputs":{"published":{"value":{"web/web":"10.10.10.7","web":"10.10.10.7:8080"}}},"resources":[{"instances":[{}]}]}"#);
    let (o, traced) = crate::testing::traced(async { observed(&proxmox_manifest(), &tf) }).await;
    assert!(o.published.is_empty(), "{:?}", o.published);
    assert!(
        traced.contains("WARN") && traced.contains("`published` entry") && traced.contains("10.10.10.7:8080"),
        "{traced}"
    );
}

#[test]
fn a_tainted_resource_leaves_the_machines_unhealthy() {
    // A machine's create or provisioner failed: the VMs exist, the run never came up.
    let tf = state(r#"{"resources":[{"instances":[{"status":"tainted"}]}]}"#);
    let o = observed(&proxmox_manifest(), &tf);
    assert!(o.machines.iter().all(|x| x.state == MachineState::Unhealthy));
}

#[test]
fn removing_what_isnt_there_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    remove(&dir.path().join("missing")).unwrap();
    std::fs::create_dir_all(dir.path().join(".terraform/providers")).unwrap();
    std::fs::write(dir.path().join(LOCK_FILE), "{}").unwrap();
    remove(&dir.path().join(".terraform")).unwrap();
    remove(&dir.path().join(LOCK_FILE)).unwrap();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}
