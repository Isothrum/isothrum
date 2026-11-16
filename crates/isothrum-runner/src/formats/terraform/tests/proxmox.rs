//! A Proxmox VE module's VMs as their server has them.

use super::*;

/// A state of two Proxmox VE VMs (bpg's), `web` (101) and `db` (102), on node `pve`.
const PROXMOX_STATE: &str = r#"{"format_version":"1.0","values":{"root_module":{"resources":[
    {"address":"proxmox_virtual_environment_vm.web","mode":"managed","type":"proxmox_virtual_environment_vm","values":{"id":"101","node_name":"pve","vm_id":101}},
    {"address":"proxmox_virtual_environment_vm.db","mode":"managed","type":"proxmox_virtual_environment_vm","values":{"id":"102","node_name":"pve","vm_id":102}}
]}}}"#;

/// The server and the token a Proxmox VE run gives, as bpg/proxmox reads them.
const PROXMOX_RUN: [(&str, &str); 2] = [("PROXMOX_VE_ENDPOINT", "https://pve.lab:8006"), ("PROXMOX_VE_API_TOKEN", "root@pam!lab=t0ken")];

/// Terraform answering the Proxmox state, and Proxmox VE's API (through curl) answering each
/// request by the URL its config names: `listing` for `cluster/resources`, `free_id` (the id
/// `cluster/nextid` gives back, or a refusal) for `nextid`.
fn proxmox_model(listing: &'static str, free_id: Option<&'static str>) -> FakeProcesses {
    FakeProcesses::model(move |call| {
        if call.program == "terraform" {
            return match call.args.first().map(String::as_str) {
                Some("show") => Reply::ok(PROXMOX_STATE),
                Some("output") => Reply::ok("{}"),
                _ => panic!("unscripted `{}`", call.line()),
            };
        }
        assert_eq!(call.program, "curl", "{}", call.line());
        let config = std::fs::read_to_string(call.stdin.as_ref().expect("a config piped in")).expect("its config");
        assert!(config.contains("PVEAPIToken=root@pam!lab=t0ken"), "signed in with the run's token: {config}");
        assert!(!config.contains("request = \"POST\""), "a read is a GET: {config}");
        if config.contains("cluster/resources?type=vm") {
            Reply::ok(&format!("{listing}\n200"))
        } else if config.contains("cluster/nextid?vmid=102") {
            match free_id {
                Some(id) => Reply::ok(&format!("{{\"data\":\"{id}\"}}\n200")),
                None => Reply::ok("{\"data\":null,\"errors\":{\"vmid\":\"VM 102 already exists\"}}\n400"),
            }
        } else {
            panic!("an unscripted request: {config}")
        }
    })
}

/// The machines' states `observe` reads for the Proxmox state, the run given `given`.
async fn proxmox_states(fake: &FakeProcesses, given: &[(&str, &str)]) -> Result<Vec<(String, MachineState)>> {
    let lab = lab().with_state();
    let (seen, _) = with_cx_given(&lab, fake, given, async |cx| Terraform.observe(cx).await).await?;
    Ok(seen.machines.into_iter().map(|m| (m.name, m.state)).collect())
}

/// Regression: a Proxmox VE VM removed or powered off in Proxmox's own UI read as running, since
/// Terraform's state changes only at its next apply: the server is asked, with the run's token.
#[tokio::test]
async fn a_proxmox_vm_powered_off_or_removed_on_its_server_reads_so() {
    let listing = r#"{"data":[{"vmid":101,"node":"pve","status":"stopped","type":"qemu"}]}"#;
    let states = proxmox_states(&proxmox_model(listing, Some("102")), &PROXMOX_RUN).await.expect("observed");
    assert_eq!(states, [("web".to_owned(), MachineState::Stopped), ("db".to_owned(), MachineState::Absent)]);
}

/// A VM the listing leaves out while its id is taken is one the credentials may not audit: the
/// module's state stands.
#[tokio::test]
async fn a_proxmox_vm_hidden_from_the_listing_but_holding_its_id_keeps_its_state() {
    let listing = r#"{"data":[{"vmid":"101","node":"pve","status":"running","type":"qemu"}]}"#;
    let states = proxmox_states(&proxmox_model(listing, None), &PROXMOX_RUN).await.expect("observed");
    assert_eq!(states, [("web".to_owned(), MachineState::Running), ("db".to_owned(), MachineState::Running)]);
}

/// Without the run's token, nothing asks the server: the module's state stands.
#[tokio::test]
async fn a_proxmox_run_without_credentials_reads_its_state_only() {
    let fake = proxmox_model("{}", None);
    let states = proxmox_states(&fake, &PROXMOX_RUN[..1]).await.expect("observed");
    assert_eq!(
        states.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
        [MachineState::Running, MachineState::Running]
    );
    assert_eq!(fake.made("curl").len(), 0);
}

/// A server that refuses the token is a refused sign-in, never a run read as it stood.
#[tokio::test]
async fn a_proxmox_server_refusing_the_token_is_server_auth_failed() {
    let fake = FakeProcesses::script()
        .expect("terraform show", Reply::ok(PROXMOX_STATE))
        .allow("terraform output", Reply::ok("{}"))
        .expect("curl", Reply::ok("{\"data\":null}\n401"))
        .build();
    let e = proxmox_states(&fake, &PROXMOX_RUN).await.expect_err("refused");
    assert_eq!(e.code(), ErrorCode::ServerAuthFailed, "{e}");
}
