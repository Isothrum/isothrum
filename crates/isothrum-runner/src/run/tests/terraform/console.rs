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

/// A server up with the fake Proxmox API, `api` answering its `curl`s.
async fn up_with_api(api: impl Fn(&Call) -> Option<Reply> + Send + Sync + 'static) -> super::Server {
    let s = scripted(HostOs::Linux, SOLO_WEB_PROXMOX, move |call| {
        if call.line().contains(" apply ") {
            let state = format!(
                r#"{{"outputs":{{"ip":{{"value":"{ROUTER}"}},"ssh_user":{{"value":"isoloom"}},"ready_file":{{"value":"/var/lib/isoloom/ready"}},"vm_id_web":{{"value":104}}}},"resources":[{{"instances":[{{}}]}}]}}"#
            );
            std::fs::write(state_dir(call).unwrap().join("terraform.tfstate"), state).unwrap();
            return Some(Reply::ok("Apply complete!"));
        }
        api(call)
    });
    s.runner.open(&s.manifest, None).unwrap().up(&opts("pve-run"), &Recorder::new()).await.unwrap();
    s
}

/// The run on `s`, its connection giving Proxmox's password.
fn signed_in(s: &super::Server) -> crate::Running<'_> {
    let mut connection = Secrets::new();
    connection.insert("proxmox_password", SecretValue::new("pw-s3cret"));
    s.runner.open(&s.manifest, None).unwrap().with_connection(connection)
}

#[tokio::test(start_paused = true)]
async fn a_proxmox_vms_console_is_proxmoxs_own_through_its_api() {
    let configs = Arc::new(Mutex::new(Vec::new()));
    let s = up_with_api({
        let configs = Arc::clone(&configs);
        move |call| proxmox_api(call, &configs)
    })
    .await;
    let run = signed_in(&s);
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

#[tokio::test(start_paused = true)]
async fn a_console_request_cut_short_leaves_no_credentials_on_disk() {
    // Regression: the config was removed only once `curl` returned, so a request dropped while it
    // ran (a cancel, Ctrl-C) left the password in the run's state folder.
    let configs = Arc::new(Mutex::new(Vec::new()));
    let s = up_with_api({
        let configs = Arc::clone(&configs);
        move |call| proxmox_api(call, &configs).map(|r| r.after(std::time::Duration::from_secs(3600)))
    })
    .await;
    let run = signed_in(&s);
    let cut = tokio::time::timeout(std::time::Duration::from_secs(10), run.graphical_console("web")).await;
    assert!(cut.is_err(), "the request was still running");
    let said = configs.lock().unwrap_or_else(PoisonError::into_inner).clone();
    assert!(said[0].contains("pw-s3cret"), "curl was given the password: {said:?}");
    let config = s.runner.store.run_dir(run.name()).unwrap().join("proxmox-console.curl");
    assert!(!config.exists(), "the credentials are gone");
}

/// A Proxmox VE VM opens in Proxmox VE's own web console, at the server the run's address names,
/// on the VM's node and id; the status names it, the default browser all it needs.
#[tokio::test(start_paused = true)]
async fn a_proxmox_vm_opens_in_proxmoxs_web_console() {
    let s = up_with_api(|_| None).await;
    let run = signed_in(&s);
    let status = run.status().await.unwrap();
    assert_eq!(
        status.native.map(|a| (a.name, a.needs)),
        Some(("Proxmox VE".into(), vec![crate::NativeNeed::Browser]))
    );
    let native = run.native_console("web").await.unwrap();
    let [crate::NativeOpen::Url { url }] = native.ways.as_slice() else {
        panic!("{:?}", native.ways)
    };
    assert!(
        url.starts_with("https://") && url.ends_with("/?console=kvm&novnc=1&vmid=104&node=pve-run"),
        "{url}"
    );
}
