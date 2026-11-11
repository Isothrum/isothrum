//! A Proxmox VE VM's console, through its API (`source.from: proxmox_api`): `vncproxy` asked of
//! the VM with `curl`, signed in with the run's connection (an API token, else the user's
//! password through a ticket), its answer turned into the API's VNC websocket and the ticket that
//! opens it ([`ProxmoxVnc`]). The credentials reach `curl` in a config file the runner writes
//! owner-only in the run's state folder, pipes into it, and removes: never on its command line.

use std::time::Duration;

use isoloom_manifest::{ConsoleSource, Machine};

use super::none;
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;
use crate::secrets::SecretValue;

/// How long one API call may go without progress.
const LIMIT: Duration = Duration::from_secs(30);

/// The config file `curl` reads, in the run's state folder, only while it runs.
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
        return Err(none(machine, "it has no Proxmox console"));
    };
    let at = |f: &str| format!("machines.{}.consoles.source.{f}", machine.name);
    let base = cx.vars.render(endpoint, &at("endpoint"))?.trim_end_matches('/').to_owned();
    let (node, vm_id) = (cx.vars.render(node, &at("node"))?, cx.vars.render(vm_id, &at("vm_id"))?);
    let insecure = cx.vars.render(insecure, &at("insecure"))?.trim() == "true";
    let host = base
        .strip_prefix("https://")
        .ok_or_else(|| none(machine, format!("the Proxmox API `{base}` isn't https")))?;
    if !plain(&node) || !vm_id.chars().all(|c| c.is_ascii_digit()) || vm_id.is_empty() {
        return Err(none(machine, format!("its VM `{vm_id}` on node `{node}` can't be named in a request")));
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
        return Err(none(machine, "Proxmox's vncproxy gave no port and ticket"));
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
                _ => Err(none(machine, "Proxmox gave no ticket for the user's password")),
            }
        }
        (None, None) => Err(none(machine, "the run's connection gives no Proxmox API token or password")),
    }
}

/// The `data` of a POST to `url`, its `fields` (the config lines that sign it in and its form)
/// in the config file `curl` reads, removed once it ran.
async fn api(cx: &Cx<'_>, machine: &Machine, url: &str, insecure: bool, fields: &[String]) -> Result<serde_json::Value> {
    let config = cx.state_dir().join(CONFIG);
    let mut text = format!("url = {}\nrequest = \"POST\"\n", quoted(url));
    for f in fields {
        text.push_str(f);
        text.push('\n');
    }
    crate::store::write(&config, text.as_bytes())?;
    let mut args = vec!["--silent", "--show-error", "--fail-with-body", "--max-time", "25", "--config", "-"];
    if insecure {
        args.push("--insecure");
    }
    let mut spec = ProcessSpec::new("curl", args);
    spec.stdin = Some(config.clone());
    let out = call(cx, "console", &spec, LIMIT, Echo::Quiet).await;
    crate::store::remove(&config)?;
    let out = out?;
    if !out.success() {
        return Err(none(machine, format!("Proxmox's API refused: {}", cx.events.mask(&out.reason()))));
    }
    let body: serde_json::Value = serde_json::from_str(&out.stdout.join("\n")).map_err(|e| Error::NoConsole {
        machine: machine.name.to_string(),
        why: format!("Proxmox's API answered no JSON: {e}"),
    })?;
    Ok(body.get("data").cloned().unwrap_or(serde_json::Value::Null))
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
