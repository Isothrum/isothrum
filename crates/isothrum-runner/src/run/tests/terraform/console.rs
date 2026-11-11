//! A Proxmox VM's graphical console through the fake Proxmox API: `vncproxy` asked with `curl`,
//! its credentials in the config piped into it, never on its command line.

use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::HostOs;

use super::{ROUTER, opts, scripted, state_dir};
use crate::process::fake::{Call, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::SOLO_WEB_PROXMOX;
use crate::{ErrorCode, Recorder};

/// The fake Proxmox API's answers to `curl`, by the request's URL, and every config `curl` read.
fn proxmox_api(call: &Call, configs: &Mutex<Vec<String>>) -> Option<Reply> {
    if call.program != "curl" {
        return None;
    }
    let config = std::fs::read_to_string(call.stdin.as_ref()?).unwrap();
    configs.lock().unwrap_or_else(PoisonError::into_inner).push(config.clone());
    if config.contains("/access/ticket") {
        return Some(Reply::ok(
            r#"{"data":{"ticket":"PVE:root@pam:ABC::sig","CSRFPreventionToken":"csrf1","username":"root@pam"}}"#,
        ));
    }
    if config.contains("/nodes/pve-run/qemu/104/vncproxy") {
        return Some(Reply::ok(
            r#"{"data":{"port":"5901","ticket":"PVEVNC:6A3F::tk+t/x=","user":"root@pam","cert":"c","upid":"u"}}"#,
        ));
    }
    Some(Reply::fail(22, "curl: (22) The requested URL returned error: 401"))
}

#[tokio::test(start_paused = true)]
async fn a_proxmox_vms_console_is_proxmoxs_own_through_its_api() {
    let configs = Arc::new(Mutex::new(Vec::new()));
    let s = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, {
        let configs = Arc::clone(&configs);
        move |call| {
            if call.line().contains(" apply ") {
                let state = format!(
                    r#"{{"outputs":{{"ip":{{"value":"{ROUTER}"}},"ssh_user":{{"value":"isoloom"}},"ready_file":{{"value":"/var/lib/isoloom/ready"}},"vm_id_web":{{"value":104}}}},"resources":[{{"instances":[{{}}]}}]}}"#
                );
                std::fs::write(state_dir(call).unwrap().join("terraform.tfstate"), state).unwrap();
                return Some(Reply::ok("Apply complete!"));
            }
            proxmox_api(call, &configs)
        }
    });
    s.runner.open(&s.manifest, None).unwrap().up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    let mut connection = Secrets::new();
    connection.insert("proxmox_password", SecretValue::new("pw-s3cret"));
    let run = s.runner.open(&s.manifest, None).unwrap().with_connection(connection);
    let console = run.graphical_console("web").await.unwrap();
    let crate::Endpoint::ProxmoxVnc(vnc) = &console.endpoint else {
        panic!("{console:?}")
    };
    assert_eq!(
        vnc.websocket.expose(),
        "wss://172.16.1.10:8006/api2/json/nodes/pve-run/qemu/104/vncwebsocket?port=5901&vncticket=PVEVNC%3A6A3F%3A%3Atk%2Bt%2Fx%3D"
    );
    assert_eq!((vnc.port, vnc.ticket.expose(), vnc.insecure), (5901, "PVEVNC:6A3F::tk+t/x=", false));
    assert_eq!(vnc.authorization.0, "Cookie");
    assert_eq!(
        console.password.as_ref().map(SecretValue::expose),
        Some("PVEVNC:6A3F::tk+t/x="),
        "the ticket is the VNC password"
    );
    // The credentials reached curl in its config, never its command line, and the config is gone.
    let said = configs.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(said[0].contains("data-urlencode = \"password=pw-s3cret\""), "{said:?}");
    assert!(
        said[1].contains("cookie = \"PVEAuthCookie=PVE:root@pam:ABC::sig\"") && said[1].contains("CSRFPreventionToken: csrf1"),
        "{said:?}"
    );
    let curls: Vec<Call> = s.fake.calls().into_iter().filter(|c| c.program == "curl").collect();
    assert!(curls.iter().all(|c| !c.line().contains("s3cret") && !c.line().contains("PVE:")), "{curls:?}");
    assert!(!curls[0].stdin.as_ref().unwrap().exists(), "the config is removed");
    // With no credential, no request: why instead.
    let bare = s.runner.open(&s.manifest, None).unwrap();
    let err = bare.graphical_console("web").await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::NoConsole);
    assert!(
        err.to_string().ends_with("the run's connection gives no Proxmox API token or password"),
        "{err}"
    );
}
