//! A module's Proxmox VE VM's screen: its VNC websocket through the server's API (`vncproxy`),
//! and its web console, the VM's node and id read from the module's state.
//!
//! The server and how to sign in are what the module's Proxmox provider reads itself, from the
//! run's values and secrets (they reach the tool in its environment): bpg/proxmox's
//! `PROXMOX_VE_ENDPOINT`, `PROXMOX_VE_API_TOKEN` (else `PROXMOX_VE_USERNAME` and
//! `PROXMOX_VE_PASSWORD`) and `PROXMOX_VE_INSECURE`, or Telmate/proxmox's `PM_API_URL`,
//! `PM_API_TOKEN_ID` and `PM_API_TOKEN_SECRET` (else `PM_USER` and `PM_PASS`) and
//! `PM_TLS_INSECURE`. A module that takes its server as a variable instead says it in an output
//! named `proxmox_endpoint`.

use std::time::Duration;

use super::Module;
use crate::Result;
use crate::cli::CliError;
use crate::cli::proxmox::{self, Api, Credentials};
use crate::cli::terraform::{self as tf, StateAt};
use crate::error::Error;
use crate::events::Stream;
use crate::formats::{Seen, SeenMachine, ToolCx};
use crate::graphical::native::host_name;
use crate::graphical::{Console, ConsoleOffer, ConsoleProtocol, Endpoint, NativeApp, NativeConsole, NativeNeed, NativeOpen, ProxmoxVnc, no_console};
use crate::secrets::SecretValue;

/// How long a read of the state, or a request to the API, may take.
const READ_LIMIT: Duration = Duration::from_secs(60);

/// The resource types that are a Proxmox VE VM: bpg's and Telmate's.
const PROXMOX_VMS: [&str; 2] = ["proxmox_virtual_environment_vm", "proxmox_vm_qemu"];

/// The output a module names its Proxmox server in, when its provider doesn't read it from the
/// environment.
const ENDPOINT_OUTPUT: &str = "proxmox_endpoint";

/// The app a Proxmox VE VM opens in.
const APP: &str = "Proxmox VE";

/// Whether a machine of `kind` is a Proxmox VE VM.
pub(super) fn is_proxmox(kind: Option<&str>) -> bool {
    kind.is_some_and(|k| PROXMOX_VMS.contains(&k))
}

/// What a status says of `machine`'s console: Proxmox's VNC for its VMs (not opened by a status),
/// none for any other.
pub(super) fn offer(machine: &SeenMachine) -> Option<ConsoleOffer> {
    is_proxmox(machine.kind.as_deref()).then(|| ConsoleOffer::of(ConsoleProtocol::Vnc, Some("through Proxmox VE's API")))
}

/// The app the module's VMs open in: Proxmox VE's web console, when it makes any.
pub(super) fn app(seen: &Seen) -> Option<NativeApp> {
    seen.machines
        .iter()
        .any(|m| is_proxmox(m.kind.as_deref()))
        .then(|| NativeApp::new(APP, vec![NativeNeed::Browser]))
}

/// The server `machine`'s VM is on, and its node and id there, from the run's values and the
/// module's state and outputs.
async fn target(cx: &ToolCx<'_>, machine: &str) -> Result<(Api, String, String)> {
    let module = Module::of(cx);
    let state = module.state()?;
    let held = module.shown(state).await?;
    let vm = super::vm_named(cx, &held, machine)?;
    if !is_proxmox(Some(&vm.kind)) {
        return Err(no_console(machine, format!("a `{}` gives no screen this runner reaches", vm.kind)));
    }
    let (Some(node), Some(id)) = (vm.vm.node.clone(), vm.vm.vm_id.clone()) else {
        return Err(no_console(machine, "the module's state names no Proxmox VE node and VM id for it"));
    };
    if !host_name(&node) || id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return Err(no_console(machine, "its Proxmox VE node or VM id isn't one"));
    }
    let endpoint = match cx.var("PROXMOX_VE_ENDPOINT").or_else(|| cx.var("PM_API_URL")) {
        Some(e) => e,
        None => endpoint_output(&module, state).await?.ok_or_else(|| {
            no_console(
                machine,
                "the run names no Proxmox VE server (`PROXMOX_VE_ENDPOINT`, `PM_API_URL`, or the module's `proxmox_endpoint` output)",
            )
        })?,
    };
    let insecure = ["PROXMOX_VE_INSECURE", "PM_TLS_INSECURE"]
        .iter()
        .any(|v| cx.var(v).is_some_and(|v| matches!(v.trim(), "true" | "1")));
    let api = Api::at(&endpoint, insecure)
        .ok_or_else(|| no_console(machine, format!("its Proxmox VE address `{}` isn't an https server", endpoint.escape_debug())))?;
    Ok((api, node, id))
}

/// The module's `proxmox_endpoint` output, when it has one that is text.
async fn endpoint_output(module: &Module<'_, '_>, state: StateAt<'_>) -> Result<Option<String>> {
    if matches!(state, StateAt::File(file) if !file.exists()) {
        return Ok(None);
    }
    let quiet = |_: Stream, _: &str| {};
    let outputs = tf::outputs(&module.call(READ_LIMIT, &quiet), state)
        .await
        .map_err(|e| module.error("output", e))?;
    Ok(outputs.get(ENDPOINT_OUTPUT).and_then(|o| o.value.as_str()).map(str::to_owned))
}

/// How the run signs in to the API, as its provider would.
fn credentials(cx: &ToolCx<'_>) -> Option<Credentials> {
    if let Some(token) = cx.var("PROXMOX_VE_API_TOKEN") {
        return Some(Credentials::Token(SecretValue::new(token)));
    }
    if let (Some(id), Some(secret)) = (cx.var("PM_API_TOKEN_ID"), cx.var("PM_API_TOKEN_SECRET")) {
        return Some(Credentials::Token(SecretValue::new(format!("{id}={secret}"))));
    }
    let password = |user: &str, password: &str| {
        Some(Credentials::Password {
            user: cx.var(user)?,
            password: SecretValue::new(cx.var(password)?),
        })
    };
    password("PROXMOX_VE_USERNAME", "PROXMOX_VE_PASSWORD").or_else(|| password("PM_USER", "PM_PASS"))
}

/// The runner's error for the API's `e`, on `machine`'s console at `api`: a refused sign-in as
/// such, a missing `curl` as a missing tool, anything else as no console.
fn api_error(machine: &str, api: &Api, e: CliError<proxmox::Failure>) -> Error {
    match e {
        CliError::Failed {
            failure: proxmox::Failure::Refused(status),
            ..
        } => Error::ServerAuthFailed {
            step: "console".to_owned(),
            server: Some(api.server().to_owned()),
            reason: format!("HTTP {status}"),
        },
        CliError::Missing { tool } => Error::ToolMissing {
            step: "console".to_owned(),
            tool: tool.to_owned(),
        },
        other => no_console(machine, format!("Proxmox VE's API didn't open it: {other}")),
    }
}

/// `machine`'s VNC websocket, `vncproxy` asked of its VM.
pub(super) async fn reach(cx: &ToolCx<'_>, machine: &str) -> Result<Console> {
    let (api, node, id) = target(cx, machine).await?;
    let credentials = credentials(cx).ok_or_else(|| {
        no_console(
            machine,
            "the run gives no Proxmox VE API token or password (`PROXMOX_VE_API_TOKEN`, `PM_API_TOKEN_ID` and `PM_API_TOKEN_SECRET`, or a user and password)",
        )
    })?;
    crate::store::create_dir(cx.state_dir)?;
    let quiet = |_: Stream, _: &str| {};
    let module = Module::of(cx);
    let call = module.call(READ_LIMIT, &quiet);
    let sign_in = proxmox::sign_in(&call, &api, &credentials, cx.state_dir)
        .await
        .map_err(|e| api_error(machine, &api, e))?;
    let vnc = proxmox::vncproxy(&call, &api, &sign_in, (&node, &id), cx.state_dir)
        .await
        .map_err(|e| api_error(machine, &api, e))?;
    let websocket = format!(
        "wss://{}{}/vncwebsocket?port={}&vncticket={}",
        api.server(),
        proxmox::vm_path(&node, &id),
        vnc.port,
        crate::encoding::percent_encoded(vnc.ticket.expose())
    );
    let endpoint = Endpoint::ProxmoxVnc(ProxmoxVnc {
        websocket: SecretValue::new(websocket),
        port: vnc.port,
        ticket: vnc.ticket.clone(),
        authorization: sign_in.header(),
        insecure: api.insecure,
    });
    let mut console = Console::new(machine, ConsoleProtocol::Vnc, endpoint);
    console.password = Some(vnc.ticket);
    Ok(console)
}

/// Proxmox VE's web console of `machine`'s VM: `<server>/?console=kvm&novnc=1&vmid=<id>&node=<node>`.
pub(super) async fn native(cx: &ToolCx<'_>, machine: &str) -> Result<NativeConsole> {
    let (api, node, id) = target(cx, machine).await?;
    let app = NativeApp::new(APP, vec![NativeNeed::Browser]);
    let url = format!("{}/?console=kvm&novnc=1&vmid={id}&node={node}", api.base);
    Ok(NativeConsole::new(machine, &app, vec![NativeOpen::Url { url }]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_proxmox_vm_offers_a_screen() {
        let m = |kind: &str| SeenMachine {
            name: "web".into(),
            state: crate::MachineState::Running,
            published: Vec::new(),
            kind: Some(kind.into()),
            address: None,
            windows: false,
        };
        assert_eq!(offer(&m("proxmox_vm_qemu")).map(|o| o.protocol), Some(ConsoleProtocol::Vnc));
        assert_eq!(offer(&m("aws_instance")), None);
        let seen = Seen {
            machines: vec![m("aws_instance"), m("proxmox_virtual_environment_vm")],
            ..Seen::default()
        };
        assert_eq!(app(&seen).map(|a| a.name).as_deref(), Some(APP));
        assert_eq!(app(&Seen::default()), None);
    }
}
