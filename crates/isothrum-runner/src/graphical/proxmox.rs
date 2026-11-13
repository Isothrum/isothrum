//! A Proxmox VE VM's console, through its API (`source.from: proxmox_api`): `vncproxy` asked of
//! the VM with `curl`, signed in with the run's connection (an API token, else the user's
//! password through a ticket), its answer turned into the API's VNC websocket and the ticket that
//! opens it ([`ProxmoxVnc`]). The credentials reach `curl` in a config file the runner writes
//! owner-only in the run's state folder, pipes into it, and removes however the request ends
//! (a cancel, Ctrl-C included): never on its command line.

use std::time::Duration;

use isoloom_manifest::{ConsoleSource, Machine};

use super::no_console;
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;
use crate::secrets::SecretValue;
use crate::store::Transient;

/// How long one API call may go without progress.
const LIMIT: Duration = Duration::from_secs(30);

/// The config file `curl` reads, in the run's state folder, only while it runs ([`Transient`]).
const CONFIG: &str = "proxmox-console.curl";

/// Proxmox VE's VNC websocket for one VM, opened for one session: connect to `websocket` with
/// the `authorization` header (TLS unverified when `insecure`), then speak VNC with `ticket` as
/// the password. Every secret is a [`SecretValue`]: never print or log one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxmoxVnc {
    /// `wss://<server>:8006/api2/json/nodes/<node>/qemu/<id>/vncwebsocket?port=<port>&vncticket=<ticket>`:
    /// it holds the ticket.
    pub websocket: SecretValue,
    /// The VNC port `vncproxy` opened on the node (in the URL already).
    pub port: u16,
    /// The ticket: the VNC password, and in the URL.
    pub ticket: SecretValue,
    /// The header that signs the websocket in: its name (`Authorization`, `Cookie`) and value.
    pub authorization: (String, SecretValue),
    /// The server's certificate isn't verified (a self-signed one: the run's `proxmox_insecure`).
    pub insecure: bool,
}

/// How the API is signed in to.
enum SignIn {
    /// An API token: `Authorization: PVEAPIToken=<token>`.
    Token(String),
    /// A ticket from `access/ticket`: the cookie, and the CSRF header a POST needs.
    Ticket { ticket: String, csrf: String },
}

impl SignIn {
    /// The config lines that sign a request in.
    fn lines(&self, post: bool) -> Vec<String> {
        match self {
            SignIn::Token(t) => vec![format!("header = {}", quoted(&format!("Authorization: PVEAPIToken={t}")))],
            SignIn::Ticket { ticket, csrf } => {
                let mut out = vec![format!("cookie = {}", quoted(&format!("PVEAuthCookie={ticket}")))];
                if post {
                    out.push(format!("header = {}", quoted(&format!("CSRFPreventionToken: {csrf}"))));
                }
                out
            }
        }
    }

    /// The websocket's header.
    fn header(&self) -> (String, SecretValue) {
        match self {
            SignIn::Token(t) => ("Authorization".into(), SecretValue::new(format!("PVEAPIToken={t}"))),
            SignIn::Ticket { ticket, .. } => ("Cookie".into(), SecretValue::new(format!("PVEAuthCookie={ticket}"))),
        }
    }
}

/// `machine`'s console as `console` (a `proxmox_api` one) says, `vncproxy` asked for it.
pub(super) async fn vnc(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console) -> Result<ProxmoxVnc> {
    let Some(ConsoleSource::ProxmoxApi {
        endpoint,
        node,
        vm_id,
        insecure,
        username,
        token,
        password,
    }) = &console.source
    else {
        return Err(no_console(machine, "it has no Proxmox console"));
    };
    let at = |f: &str| format!("machines.{}.consoles.source.{f}", machine.name);
    let base = cx.vars.render(endpoint, &at("endpoint"))?.trim_end_matches('/').to_owned();
    let (node, vm_id) = (cx.vars.render(node, &at("node"))?, cx.vars.render(vm_id, &at("vm_id"))?);
    let insecure = cx.vars.render(insecure, &at("insecure"))?.trim() == "true";
    let host = base
        .strip_prefix("https://")
        .ok_or_else(|| no_console(machine, format!("the Proxmox API `{base}` isn't https")))?;
    if !plain(&node) || !vm_id.chars().all(|c| c.is_ascii_digit()) || vm_id.is_empty() {
        return Err(no_console(machine, format!("its VM `{vm_id}` on node `{node}` can't be named in a request")));
    }
    let sign_in = sign_in(cx, machine, &base, insecure, (username, token, password)).await?;
    let path = format!("/api2/json/nodes/{node}/qemu/{vm_id}");
    let mut fields = sign_in.lines(true);
    fields.push("data = \"websocket=1\"".to_owned());
    let said = api(cx, machine, &format!("{base}{path}/vncproxy"), insecure, &fields).await?;
    let port = said
        .get("port")
        .and_then(|p| p.as_u64().or_else(|| p.as_str()?.parse().ok()))
        .and_then(|p| u16::try_from(p).ok());
    let ticket = said.get("ticket").and_then(serde_json::Value::as_str);
    let (Some(port), Some(ticket)) = (port, ticket) else {
        return Err(no_console(machine, "Proxmox's vncproxy gave no port and ticket"));
    };
    cx.events.hide([ticket.to_owned()]);
    Ok(ProxmoxVnc {
        websocket: SecretValue::new(format!("wss://{host}{path}/vncwebsocket?port={port}&vncticket={}", encoded(ticket))),
        port,
        ticket: SecretValue::new(ticket),
        authorization: sign_in.header(),
        insecure,
    })
}

/// How the API is signed in to: the run's connection's API token (`token`), else the user's
/// password (`password`) traded for a ticket.
async fn sign_in(
    cx: &Cx<'_>,
    machine: &Machine,
    base: &str,
    insecure: bool,
    (username, token, password): (&isoloom_manifest::Template, &isoloom_manifest::Identifier, &isoloom_manifest::Identifier),
) -> Result<SignIn> {
    let value = |slot: &isoloom_manifest::Identifier| cx.secrets.get(slot.as_str()).map(|v| v.expose().to_owned()).filter(|v| !v.is_empty());
    match (value(token), value(password)) {
        (Some(t), _) => Ok(SignIn::Token(t)),
        (None, Some(p)) => {
            let user = cx.vars.render(username, &format!("machines.{}.consoles.source.username", machine.name))?;
            let fields = [
                format!("data-urlencode = {}", quoted(&format!("username={user}"))),
                format!("data-urlencode = {}", quoted(&format!("password={p}"))),
            ];
            let said = api(cx, machine, &format!("{base}/api2/json/access/ticket"), insecure, &fields).await?;
            let ticket = said.get("ticket").and_then(serde_json::Value::as_str);
            let csrf = said.get("CSRFPreventionToken").and_then(serde_json::Value::as_str);
            match (ticket, csrf) {
                (Some(t), Some(c)) => Ok(SignIn::Ticket {
                    ticket: t.to_owned(),
                    csrf: c.to_owned(),
                }),
                _ => Err(no_console(machine, "Proxmox gave no ticket for the user's password")),
            }
        }
        (None, None) => Err(no_console(machine, "the run's connection gives no Proxmox API token or password")),
    }
}

/// The `data` of a POST to `url`, its `fields` (the config lines that sign it in and its form)
/// in the config file `curl` reads, removed once it ran or the request is cut short.
async fn api(cx: &Cx<'_>, machine: &Machine, url: &str, insecure: bool, fields: &[String]) -> Result<serde_json::Value> {
    let out = request(cx, "console", (url, Some("POST")), insecure, fields).await?;
    if !out.success() {
        return Err(no_console(machine, format!("Proxmox's API refused: {}", cx.events.mask(&out.reason()))));
    }
    let body: serde_json::Value = serde_json::from_str(&out.stdout.join("\n")).map_err(|e| Error::NoConsole {
        machine: machine.name.to_string(),
        why: format!("Proxmox's API answered no JSON: {e}"),
    })?;
    Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
}

/// What `curl` answered to a request to `url` (`method`: `POST`, or a GET), for step `step`, its
/// `fields` in the config file it reads, removed once it ran or the request is cut short.
async fn request(cx: &Cx<'_>, step: &str, (url, method): (&str, Option<&str>), insecure: bool, fields: &[String]) -> Result<crate::process::Captured> {
    let mut text = format!("url = {}\n", quoted(url));
    if let Some(method) = method {
        text.push_str("request = ");
        text.push_str(&quoted(method));
        text.push('\n');
    }
    for f in fields {
        text.push_str(f);
        text.push('\n');
    }
    let config = Transient::write(cx.state_dir().join(CONFIG), text.as_bytes(), "a Proxmox API request's curl config")?;
    let mut args = vec!["--silent", "--show-error", "--fail-with-body", "--max-time", "25", "--config", "-"];
    if insecure {
        args.push("--insecure");
    }
    let mut spec = ProcessSpec::new("curl", args);
    spec.stdin = Some(config.path().to_path_buf());
    config.remove_after(call(cx, step, &spec, LIMIT, Echo::Quiet).await)
}

/// Where each VM of the run with Proxmox's console stands on its server, by machine name, as the
/// server's API says (`cluster/resources`, every VM of the cluster it lets the run's credentials
/// see, wherever it migrated): running, stopped, or absent when the server no longer has it (its
/// web UI's Remove). `vars` holds the run's params and outputs (each VM's id). Nothing for a run
/// whose VMs have no such console, no id yet, or whose connection gives no credentials; nothing
/// either when the server can't be asked (unreachable, an answer that isn't the API's), traced:
/// the run's own state then stands.
///
/// # Errors
///
/// `server_auth_failed` when the server refuses the credentials (401, 403); a value of the
/// console that can't be filled.
pub(crate) async fn vm_states(cx: &Cx<'_>, vars: &crate::vars::Vars) -> Result<std::collections::BTreeMap<String, crate::MachineState>> {
    let mut out = std::collections::BTreeMap::new();
    let vms = proxmox_vms(cx, vars)?;
    let Some((_, first)) = vms.first() else { return Ok(out) };
    // Each way the API's answer is missing is said (an empty map hid why): the run's own state
    // then stands.
    let Some(sign_in) = status_sign_in(cx, vars, first).await? else {
        tracing::debug!("no Proxmox credentials among the secrets: the VMs' states aren't asked, the run's own stand");
        return Ok(out);
    };
    let url = format!("{}/api2/json/cluster/resources?type=vm", first.base);
    let Some(answer) = asked(request(cx, "status", (&url, None), first.insecure, &sign_in.lines(false)).await) else {
        return Ok(out);
    };
    let Some(listed) = answered(cx, vars, &answer)? else {
        tracing::warn!("Proxmox's answer holds no list of resources: the run's own state stands");
        return Ok(out);
    };
    for (name, vm) in &vms {
        let found = listed.iter().find(|r| {
            r.get("vmid")
                .and_then(|v| v.as_u64().map(|n| n.to_string()).or_else(|| v.as_str().map(str::to_owned)))
                .as_deref()
                == Some(vm.id.as_str())
        });
        let state = match found.and_then(|r| r.get("status")).and_then(serde_json::Value::as_str) {
            None if found.is_none() => crate::MachineState::Absent,
            Some("running") => crate::MachineState::Running,
            Some("stopped") => crate::MachineState::Stopped,
            Some(other) => crate::MachineState::Unknown(other.to_owned()),
            None => continue,
        };
        out.insert(name.clone(), state);
    }
    Ok(out)
}

/// One VM as its Proxmox console names it, filled.
struct ApiVm<'m> {
    /// The API's address, without its last `/`.
    base: String,
    /// Its id on the server.
    id: String,
    /// The server's certificate isn't verified.
    insecure: bool,
    /// The console's sign-in: its user, token slot and password slot.
    sign_in: (
        &'m isoloom_manifest::Template,
        &'m isoloom_manifest::Identifier,
        &'m isoloom_manifest::Identifier,
    ),
}

/// The run's VMs with Proxmox's console and an id `vars` fills, by machine name.
fn proxmox_vms<'m>(cx: &Cx<'m>, vars: &crate::vars::Vars) -> Result<Vec<(String, ApiVm<'m>)>> {
    let mut out = Vec::new();
    for m in &cx.manifest().machines {
        let source = m.consoles.iter().find_map(|c| match &c.source {
            Some(s @ ConsoleSource::ProxmoxApi { .. }) => Some(s),
            _ => None,
        });
        let Some(ConsoleSource::ProxmoxApi {
            endpoint,
            vm_id,
            insecure,
            username,
            token,
            password,
            ..
        }) = source
        else {
            continue;
        };
        let at = |f: &str| format!("machines.{}.consoles.source.{f}", m.name);
        // Before its first apply (or from a state of an older module), the run has no id.
        let Ok(id) = vars.render(vm_id, &at("vm_id")) else { continue };
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let base = vars.render(endpoint, &at("endpoint"))?.trim_end_matches('/').to_owned();
        let insecure = vars.render(insecure, &at("insecure"))?.trim() == "true";
        out.push((
            m.name.to_string(),
            ApiVm {
                base,
                id,
                insecure,
                sign_in: (username, token, password),
            },
        ));
    }
    Ok(out)
}

/// How a status read signs in to the API: the connection's token, else a ticket for its password;
/// none when it gives neither (the run's state then stands).
async fn status_sign_in(cx: &Cx<'_>, vars: &crate::vars::Vars, vm: &ApiVm<'_>) -> Result<Option<SignIn>> {
    let (username, token, password) = vm.sign_in;
    let value = |slot: &isoloom_manifest::Identifier| cx.secrets.get(slot.as_str()).map(|v| v.expose().to_owned()).filter(|v| !v.is_empty());
    if let Some(t) = value(token) {
        return Ok(Some(SignIn::Token(t)));
    }
    let Some(p) = value(password) else { return Ok(None) };
    let user = vars.render(username, "consoles.source.username")?;
    let fields = [
        format!("data-urlencode = {}", quoted(&format!("username={user}"))),
        format!("data-urlencode = {}", quoted(&format!("password={p}"))),
    ];
    let url = format!("{}/api2/json/access/ticket", vm.base);
    let Some(answer) = asked(request(cx, "status", (&url, Some("POST")), vm.insecure, &fields).await) else {
        return Ok(None);
    };
    let Some(data) = data_of(cx, vars, &answer)? else { return Ok(None) };
    let field = |k: &str| data.get(k).and_then(serde_json::Value::as_str).map(str::to_owned);
    Ok(field("ticket")
        .zip(field("CSRFPreventionToken"))
        .map(|(ticket, csrf)| SignIn::Ticket { ticket, csrf }))
}

/// What `curl` answered a status read, when it ran to its end; a request that couldn't (`curl`
/// missing, the server silent past the limit) is traced, and the run's state then stands.
fn asked(done: Result<crate::process::Captured>) -> Option<crate::process::Captured> {
    done.map_err(|e| tracing::warn!(error = %e, "Proxmox's API couldn't be asked where the run's VMs stand: its state stands"))
        .ok()
}

/// The list a status read's `answer` holds; none when the API couldn't be asked (traced).
fn answered(cx: &Cx<'_>, vars: &crate::vars::Vars, answer: &crate::process::Captured) -> Result<Option<Vec<serde_json::Value>>> {
    Ok(data_of(cx, vars, answer)?.and_then(|d| d.as_array().cloned()))
}

/// The `data` of the API's `answer`; none when it didn't answer one (unreachable, not the API),
/// which is traced.
///
/// # Errors
///
/// `server_auth_failed` when it refused the credentials.
fn data_of(cx: &Cx<'_>, vars: &crate::vars::Vars, answer: &crate::process::Captured) -> Result<Option<serde_json::Value>> {
    if !answer.success() {
        let reason = cx.events.mask(&answer.reason());
        if crate::server_auth::refused(&reason) {
            return Err(Error::ServerAuthFailed {
                step: "status".into(),
                server: crate::server_auth::server_of(vars.params()),
                reason,
            });
        }
        tracing::warn!(%reason, "Proxmox's API couldn't be asked where the run's VMs stand: its state stands");
        return Ok(None);
    }
    match serde_json::from_str::<serde_json::Value>(&answer.stdout.join("\n")) {
        Ok(body) => Ok(body.get("data").cloned()),
        Err(e) => {
            tracing::warn!(error = %e, "Proxmox's API answered no JSON: the run's state stands");
            Ok(None)
        }
    }
}

/// `s` as a double-quoted value of a curl config file (`\` and `"` escaped).
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r"))
}

/// Whether `s` is a node name that goes in a URL path as it is.
fn plain(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// `s` percent-encoded for a query value: every byte but an unreserved one.
fn encoded(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ticket_is_encoded_for_the_url_and_a_value_quoted_for_curl() {
        assert_eq!(encoded("PVEVNC:6A3F::a+b/c="), "PVEVNC%3A6A3F%3A%3Aa%2Bb%2Fc%3D");
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quoted("x\ny"), "\"x\\ny\"", "a new line can't start another option");
        assert!(plain("pve-1") && !plain("pve/../x") && !plain(""));
    }

    #[test]
    fn a_token_or_a_ticket_signs_the_websocket_in() {
        let t = SignIn::Token("root@pam!x=s3cr3t".into());
        assert_eq!(t.lines(true), ["header = \"Authorization: PVEAPIToken=root@pam!x=s3cr3t\""]);
        assert_eq!(t.header().0, "Authorization");
        let k = SignIn::Ticket {
            ticket: "PVE:t".into(),
            csrf: "c".into(),
        };
        assert_eq!(k.lines(true), ["cookie = \"PVEAuthCookie=PVE:t\"", "header = \"CSRFPreventionToken: c\""]);
        assert_eq!(k.header(), ("Cookie".to_owned(), SecretValue::new("PVEAuthCookie=PVE:t")));
    }
}
