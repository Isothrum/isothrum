//! A Proxmox run's VMs as the server says they stand (tests/resilience.md M10, N4): a VM removed
//! in Proxmox's web UI reads absent, not running from Terraform's state; one powered off there
//! reads stopped; a revoked API token or a changed password is the server refusing the run's
//! credentials, whether a status read or Terraform meets it.

use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use super::console::{signed_in, up_with_api};
use super::{opts, scripted};
use crate::process::fake::{Call, Reply};
use crate::testing::SOLO_WEB_PROXMOX;
use crate::{ErrorCode, MachineState, Recorder, RunState};

/// What the fake Proxmox API's `cluster/resources` answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Server {
    /// VM 104 with this status.
    Has(&'static str),
    /// No VM 104: removed in the web UI.
    Removed,
    /// 401: the token revoked.
    Refuses,
    /// Nothing answers.
    Unreachable,
}

/// The fake API, answering as `server` says.
fn api(server: &Arc<Mutex<Server>>) -> impl Fn(&Call) -> Option<Reply> + Send + Sync + 'static {
    let server = Arc::clone(server);
    move |call| {
        if call.program != "curl" {
            return None;
        }
        let config = std::fs::read_to_string(call.stdin.as_ref()?).unwrap();
        let now = *server.lock().unwrap_or_else(PoisonError::into_inner);
        Some(match now {
            Server::Refuses => Reply::fail(22, "curl: (22) The requested URL returned error: 401"),
            Server::Unreachable => Reply::fail(7, "curl: (7) Failed to connect to 172.16.1.10 port 8006: Connection refused"),
            _ if config.contains("/access/ticket") => Reply::ok(r#"{"data":{"ticket":"PVE:t","CSRFPreventionToken":"c"}}"#),
            Server::Has(status) => Reply::ok(&format!(
                r#"{{"data":[{{"vmid":104,"node":"pve-run","status":"{status}","type":"qemu"}},{{"vmid":7,"status":"running"}}]}}"#
            )),
            Server::Removed => Reply::ok(r#"{"data":[{"vmid":7,"status":"running","type":"qemu"}]}"#),
        })
    }
}

/// `web`'s state in `status`.
fn web(status: &crate::Status) -> MachineState {
    status.machines.iter().find(|m| m.name == "web").map(|m| m.state.clone()).unwrap()
}

#[tokio::test(start_paused = true)]
async fn a_vm_removed_in_proxmoxs_web_ui_reads_gone_and_one_powered_off_there_stopped() {
    let server = Arc::new(Mutex::new(Server::Has("running")));
    let s = up_with_api(api(&server)).await;
    let run = signed_in(&s);
    let up = run.status().await.unwrap();
    assert_eq!((up.state, web(&up)), (RunState::Ready, MachineState::Running));

    // Regression: the status read Terraform's state alone, which still holds the VM.
    *server.lock().unwrap_or_else(PoisonError::into_inner) = Server::Removed;
    let removed = run.status().await.unwrap();
    assert_eq!((removed.state, web(&removed)), (RunState::Failed, MachineState::Absent));
    crate::testing::refuses_to_resume_without(&run, "web").await;

    *server.lock().unwrap_or_else(PoisonError::into_inner) = Server::Has("stopped");
    let off = run.status().await.unwrap();
    assert_eq!((off.state, web(&off)), (RunState::Failed, MachineState::Stopped));

    // The server unreachable, or no credentials to ask it with: the run's state stands.
    *server.lock().unwrap_or_else(PoisonError::into_inner) = Server::Unreachable;
    assert_eq!(web(&run.status().await.unwrap()), MachineState::Running);
    *server.lock().unwrap_or_else(PoisonError::into_inner) = Server::Removed;
    let bare = s.runner.open(&s.manifest, None).unwrap();
    assert_eq!(web(&bare.status().await.unwrap()), MachineState::Running);
}

#[tokio::test(start_paused = true)]
async fn a_revoked_token_is_the_server_refusing_the_runs_credentials() {
    let server = Arc::new(Mutex::new(Server::Has("running")));
    let s = up_with_api(api(&server)).await;
    *server.lock().unwrap_or_else(PoisonError::into_inner) = Server::Refuses;
    let err = signed_in(&s).status().await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ServerAuthFailed);
    assert!(
        matches!(&err, crate::Error::ServerAuthFailed { step, server: Some(host), .. } if step == "status" && host == "172.16.1.10"),
        "{err:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn terraform_refused_by_the_server_is_a_refused_sign_in_and_any_other_failure_its_own() {
    let refused = "Error: error retrieving VM: received an HTTP 401 response - Reason: authentication failure";
    let s = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, move |call| {
        call.line().contains(" apply ").then(|| Reply::fail(1, refused))
    });
    let run = s.runner.open(&s.manifest, None).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts("pve-run"), &events).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ServerAuthFailed, "{err}");
    assert!(err.to_string().contains("server `172.16.1.10` refused the run's credentials"), "{err}");
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(said.contains("\"code\":\"server_auth_failed\""), "{said}");
    assert_eq!(
        s.runner.store.record(run.name()).unwrap().unwrap().last.unwrap().code,
        Some(ErrorCode::ServerAuthFailed)
    );

    let other = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, |call| {
        call.line()
            .contains(" apply ")
            .then(|| Reply::fail(1, "Error: 500 Internal Server Error: storage 'local-lvm' is full"))
    });
    let failing = other.runner.open(&other.manifest, None).unwrap();
    assert_eq!(failing.up(&opts("pve-run"), &Recorder::new()).await.unwrap_err().code(), ErrorCode::ToolFailed);
}
