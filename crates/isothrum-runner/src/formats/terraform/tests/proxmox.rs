//! A Proxmox VE module's VMs as their server has them, asked over TLS, the server's self-signed
//! certificate pinned.

use super::*;
use crate::testing::https::{Request, Server, fingerprint};

/// A state of two Proxmox VE VMs (bpg's), `web` (101) and `db` (102), on node `pve`.
const PROXMOX_STATE: &str = r#"{"format_version":"1.0","values":{"root_module":{"resources":[
    {"address":"proxmox_virtual_environment_vm.web","mode":"managed","type":"proxmox_virtual_environment_vm","values":{"id":"101","node_name":"pve","vm_id":101}},
    {"address":"proxmox_virtual_environment_vm.db","mode":"managed","type":"proxmox_virtual_environment_vm","values":{"id":"102","node_name":"pve","vm_id":102}}
]}}}"#;

/// The token a Proxmox VE run gives, as bpg/proxmox reads it.
const TOKEN: &str = "root@pam!stack=t0ken";

/// Terraform answering the Proxmox state.
fn terraform() -> FakeProcesses {
    FakeProcesses::model(|call| match (call.program.as_str(), call.args.first().map(String::as_str)) {
        ("terraform", Some("show")) => Reply::ok(PROXMOX_STATE),
        ("terraform", Some("output")) => Reply::ok("{}"),
        _ => panic!("unscripted `{}`", call.line()),
    })
}

/// Proxmox VE's API answering each request by its path: `listing` for `cluster/resources`,
/// `free_id` (the id `cluster/nextid` gives back, or a refusal) for `nextid`.
fn api(listing: &'static str, free_id: Option<&'static str>) -> impl Fn(&Request) -> (u16, String) + Send + Sync + 'static {
    move |request| {
        assert_eq!(
            request.header("authorization"),
            Some(format!("PVEAPIToken={TOKEN}").as_str()),
            "signed in with the run's token"
        );
        assert_eq!(request.method, "GET", "a read is a GET");
        if request.target.ends_with("/api2/json/cluster/resources?type=vm") {
            (200, listing.to_owned())
        } else if request.target.ends_with("/api2/json/cluster/nextid?vmid=102") {
            match free_id {
                Some(id) => (200, format!("{{\"data\":\"{id}\"}}")),
                None => (400, "{\"data\":null,\"errors\":{\"vmid\":\"VM 102 already exists\"}}".to_owned()),
            }
        } else {
            panic!("an unscripted request: {request:?}")
        }
    }
}

/// The run's variables for `server`: its address, the certificate it pins (`pin`, none without
/// one), its token when `token`.
fn run(server: &Server, pin: Option<String>, token: bool) -> Vec<(String, String)> {
    let mut given = vec![("PROXMOX_VE_ENDPOINT".to_owned(), server.endpoint.clone())];
    given.extend(pin.map(|p| ("PROXMOX_VE_TLS_FINGERPRINT".to_owned(), p)));
    if token {
        given.push(("PROXMOX_VE_API_TOKEN".to_owned(), TOKEN.to_owned()));
    }
    given
}

/// The server's own certificate, pinned.
fn pinned() -> String {
    fingerprint().to_string()
}

/// The machines' states `observe` reads for the Proxmox state, the run given `given`.
async fn proxmox_states(fake: &FakeProcesses, given: &[(String, String)]) -> Result<Vec<(String, MachineState)>> {
    let stack = stack().with_state();
    let given: Vec<(&str, &str)> = given.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let (seen, _) = with_cx_given(&stack, fake, &given, async |cx| Terraform.observe(cx).await).await?;
    Ok(seen.machines.into_iter().map(|m| (m.name, m.state)).collect())
}

/// Regression: a Proxmox VE VM removed or powered off in Proxmox's own UI read as running, since
/// Terraform's state changes only at its next apply: the server is asked, with the run's token.
#[tokio::test]
async fn a_proxmox_vm_powered_off_or_removed_on_its_server_reads_so() {
    let listing = r#"{"data":[{"vmid":101,"node":"pve","status":"stopped","type":"qemu"}]}"#;
    let server = Server::start(api(listing, Some("102"))).await.unwrap();
    let states = proxmox_states(&terraform(), &run(&server, Some(pinned()), true)).await.expect("observed");
    assert_eq!(states, [("web".to_owned(), MachineState::Stopped), ("db".to_owned(), MachineState::Absent)]);
}

/// A VM the listing leaves out while its id is taken is one the credentials may not audit: the
/// module's state stands.
#[tokio::test]
async fn a_proxmox_vm_hidden_from_the_listing_but_holding_its_id_keeps_its_state() {
    let listing = r#"{"data":[{"vmid":"101","node":"pve","status":"running","type":"qemu"}]}"#;
    let server = Server::start(api(listing, None)).await.unwrap();
    let states = proxmox_states(&terraform(), &run(&server, Some(pinned()), true)).await.expect("observed");
    assert_eq!(states, [("web".to_owned(), MachineState::Running), ("db".to_owned(), MachineState::Running)]);
}

/// A server that refuses the token is a refused sign-in, never a run read as it stood.
#[tokio::test]
async fn a_proxmox_server_refusing_the_token_is_server_auth_failed() {
    let server = Server::start(|_| (401, "{\"data\":null}".to_owned())).await.unwrap();
    let e = proxmox_states(&terraform(), &run(&server, Some(pinned()), true)).await.expect_err("refused");
    assert_eq!(e.code(), ErrorCode::ServerAuthFailed, "{e}");
}

/// Nothing is sent to the server, and the module's state stands, for a run without its token,
/// a server offering another certificate than the pinned one, a self-signed one the run pins
/// none for, or a pin that isn't a fingerprint (said).
#[tokio::test]
async fn a_run_without_its_token_or_the_servers_pin_sends_it_nothing() {
    let server = Server::start(api("{}", None)).await.unwrap();
    let other = crate::tls::Fingerprint::of(b"another certificate").to_string();
    let runs = [
        run(&server, Some(pinned()), false),
        run(&server, Some(other), true),
        run(&server, None, true),
        run(&server, Some("not a pin".to_owned()), true),
    ];
    for given in runs {
        let states = proxmox_states(&terraform(), &given).await.expect("observed");
        assert_eq!(
            states.iter().map(|(_, s)| s.clone()).collect::<Vec<_>>(),
            [MachineState::Running, MachineState::Running],
            "{given:?}"
        );
    }
    assert_eq!(server.requests().len(), 0, "no request reached it");
}
