//! A Proxmox VE VM's console, through its API (`source.from: proxmox_api`): `vncproxy` asked of
//! the VM with `curl`, signed in with the run's connection (an API token, else the user's
//! password through a ticket), its answer turned into the API's VNC websocket and the ticket that
//! opens it ([`ProxmoxVnc`]). The credentials reach `curl` in a config file the runner writes
//! owner-only in the run's state folder, one per request, pipes into it, and removes however the
//! request ends (a cancel, Ctrl-C included): never on its command line.

use std::time::Duration;

use isoloom_manifest::{ConsoleSource, Machine};
use serde::de::DeserializeOwned;

use self::answer::{ClusterVm, FreeId, Ticket, VncProxy};

use super::no_console;
use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call};
use crate::process::ProcessSpec;
use crate::secrets::SecretValue;
use crate::store::TempFile;

mod answer;

/// How long one API call may go without progress.
const LIMIT: Duration = Duration::from_secs(30);

/// The config file `curl` reads, in the run's state folder, only while it runs ([`TempFile`]):
/// `proxmox-console-<random>.curl`, one per request, since a status read takes no run lock and
/// may run beside a console request.
const CONFIG: (&str, &str) = ("proxmox-console", "curl");

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

/// An API request's HTTP method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Method {
    /// A read.
    Get,
    /// A change (needs the ticket's CSRF token).
    Post,
}

impl SignIn {
    /// The sign-in `access/ticket` gave for a user's password; none without both of its fields.
    fn ticket(answer: Ticket) -> Option<SignIn> {
        Some(SignIn::Ticket {
            ticket: answer.ticket?,
            csrf: answer.csrf?,
        })
    }

    /// The config lines that sign a request by `method` in.
    fn lines(&self, method: Method) -> Vec<String> {
        match self {
            SignIn::Token(t) => vec![format!("header = {}", quoted(&format!("Authorization: PVEAPIToken={t}")))],
            SignIn::Ticket { ticket, csrf } => {
                let mut out = vec![format!("cookie = {}", quoted(&format!("PVEAuthCookie={ticket}")))];
                if method == Method::Post {
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
    let (base, insecure) = api_target(&cx.vars, (endpoint, insecure), at)?;
    let (node, vm_id) = (cx.vars.render(node, &at("node"))?, cx.vars.render(vm_id, &at("vm_id"))?);
    let host = base
        .strip_prefix("https://")
        .ok_or_else(|| no_console(machine, format!("the Proxmox API `{base}` isn't https")))?;
    if !plain(&node) || !vm_id.chars().all(|c| c.is_ascii_digit()) || vm_id.is_empty() {
        return Err(no_console(machine, format!("its VM `{vm_id}` on node `{node}` can't be named in a request")));
    }
    let sign_in = sign_in(cx, machine, &base, insecure, (username, token, password)).await?;
    let path = format!("/api2/json/nodes/{node}/qemu/{vm_id}");
    let mut fields = sign_in.lines(Method::Post);
    fields.push("data = \"websocket=1\"".to_owned());
    let said: Option<VncProxy> = api(cx, machine, &format!("{base}{path}/vncproxy"), insecure, &fields).await?;
    let port = said.as_ref().and_then(VncProxy::port);
    let ticket = said.as_ref().and_then(|s| s.ticket.as_deref());
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

/// The API a console's `endpoint` names, filled, without its last `/`, and whether its `insecure`
/// says not to verify the server's certificate (`true`); `at` names a field in errors.
fn api_target(
    vars: &crate::vars::Vars,
    (endpoint, insecure): (&isoloom_manifest::Template, &isoloom_manifest::Template),
    at: impl Fn(&str) -> String,
) -> Result<(String, bool)> {
    let base = vars.render(endpoint, &at("endpoint"))?.trim_end_matches('/').to_owned();
    let insecure = vars.render(insecure, &at("insecure"))?.trim() == "true";
    Ok((base, insecure))
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
            let said: Option<Ticket> = api(cx, machine, &format!("{base}/api2/json/access/ticket"), insecure, &fields).await?;
            said.and_then(SignIn::ticket)
                .ok_or_else(|| no_console(machine, "Proxmox gave no ticket for the user's password"))
        }
        (None, None) => Err(no_console(machine, "the run's connection gives no Proxmox API token or password")),
    }
}

/// The `data` of a POST to `url` ([`answer`]), its `fields` (the config lines that sign it in and
/// its form) in the config file `curl` reads, removed once it ran or the request is cut short.
async fn api<T: DeserializeOwned>(cx: &Cx<'_>, machine: &Machine, url: &str, insecure: bool, fields: &[String]) -> Result<Option<T>> {
    let out = request(cx, "console", (url, Method::Post), insecure, fields).await?;
    if !out.success() {
        return Err(no_console(machine, format!("Proxmox's API refused: {}", cx.events.mask(&out.reason()))));
    }
    answer::data(&out.stdout.join("\n")).map_err(|e| Error::NoConsole {
        machine: machine.name.to_string(),
        why: format!("Proxmox's API answered no JSON this runner reads: {e}"),
    })
}

/// What `curl` answered to a request to `url` by `method`, for step `step`, its
/// `fields` in the config file it reads, removed once it ran or the request is cut short.
async fn request(cx: &Cx<'_>, step: &str, (url, method): (&str, Method), insecure: bool, fields: &[String]) -> Result<crate::process::Captured> {
    let mut text = format!("url = {}\n", quoted(url));
    if method == Method::Post {
        text.push_str("request = ");
        text.push_str(&quoted("POST"));
        text.push('\n');
    }
    for f in fields {
        text.push_str(f);
        text.push('\n');
    }
    let config = TempFile::create_new_in(cx.state_dir(), CONFIG, text.as_bytes(), "a Proxmox API request's curl config")?;
    // The HTTP status on a line of its own after the body: `--fail-with-body` would say it, but
    // only from curl 7.76 on, and an older one refuses the whole command line.
    let mut args = vec!["--silent", "--show-error", "--write-out", STATUS_LINE, "--max-time", "25", "--config", "-"];
    if insecure {
        args.push("--insecure");
    }
    let mut spec = ProcessSpec::new("curl", args);
    spec.stdin = Some(config.path().to_path_buf());
    config.remove_after(call(cx, step, &spec, LIMIT, Echo::Quiet).await.map(http_answer))
}

/// What `--write-out` prints after the body: the HTTP status on a line of its own.
const STATUS_LINE: &str = "\\n%{http_code}";

/// What `curl` answered, the HTTP status line [`STATUS_LINE`] added taken off its output: an
/// answer of 400 or more fails as `--fail-with-body` fails it (exit 22, `The requested URL
/// returned error: <status>` on standard error, its body kept on standard output), so a refused
/// sign-in reads as one.
fn http_answer(mut out: crate::process::Captured) -> crate::process::Captured {
    let status = out.stdout.pop().and_then(|line| line.trim().parse::<u16>().ok());
    match status {
        Some(code) if code >= 400 && out.success() => {
            let mut stderr = out.stderr_tail.clone();
            stderr.push(format!("curl: (22) The requested URL returned error: {code}"));
            crate::process::Captured::new("curl", crate::process::Exit::new(Some(22)), out.stdout, stderr)
        }
        _ => out,
    }
}

/// Where each VM of the run with Proxmox's console stands on its server, by machine name, as the
/// server's API says (`cluster/resources`, every VM of the cluster it lets the run's credentials
/// see, wherever it migrated): running, stopped, or absent when the server no longer has it (its
/// web UI's Remove). Each server the VMs are on is asked once, as their consoles sign in to it. A
/// VM its listing leaves out is absent only when the server says no VM holds its id
/// ([`unlisted`]): the listing holds only the VMs the credentials may audit. `vars` holds the
/// run's params and outputs (each VM's id). Nothing for a run whose VMs have no such console, no
/// id yet, or whose connection gives no credentials; nothing either for a VM whose server can't be
/// asked (unreachable, an answer that isn't the API's), traced: the run's own state then stands.
///
/// # Errors
///
/// `server_auth_failed` when the server refuses the credentials (401, 403); a value of the
/// console that can't be filled.
pub(crate) async fn vm_states(cx: &Cx<'_>, vars: &crate::vars::Vars) -> Result<std::collections::BTreeMap<String, crate::MachineState>> {
    let mut out = std::collections::BTreeMap::new();
    let vms = proxmox_vms(cx, vars)?;
    let mut servers: Vec<(&ApiVm<'_>, Option<Listing>)> = Vec::new();
    for (name, vm) in &vms {
        let at = if let Some(at) = servers.iter().position(|(asked, _)| asked.same_server(vm)) {
            at
        } else {
            servers.push((vm, listing(cx, vars, vm).await?));
            servers.len() - 1
        };
        let Some(listing) = servers.get(at).and_then(|(_, l)| l.as_ref()) else {
            continue;
        };
        let state = match listing.vms.iter().find(|r| r.is(&vm.id)) {
            Some(found) => found.status.as_deref().map(reading),
            None => unlisted(cx, vm, &listing.sign_in).await,
        };
        if let Some(state) = state {
            out.insert(name.clone(), state);
        }
    }
    Ok(out)
}

/// The VMs a server lists, and the sign-in it was asked with.
struct Listing {
    /// How the server was signed in to.
    sign_in: SignIn,
    /// Its `cluster/resources` of type `vm`.
    vms: Vec<ClusterVm>,
}

/// The VMs `vm`'s server lists (`cluster/resources`), asked as `vm`'s console signs in; none when
/// there are no credentials to ask with, or no list came back (traced).
async fn listing(cx: &Cx<'_>, vars: &crate::vars::Vars, vm: &ApiVm<'_>) -> Result<Option<Listing>> {
    // Each way the API's answer is missing is said (an empty map hid why): the run's own state
    // then stands.
    let Some(sign_in) = status_sign_in(cx, vars, vm).await? else {
        tracing::debug!("no Proxmox credentials among the secrets: the VMs' states aren't asked, the run's own stand");
        return Ok(None);
    };
    let url = format!("{}/api2/json/cluster/resources?type=vm", vm.base);
    let Some(answer) = asked(request(cx, "status", (&url, Method::Get), vm.insecure, &sign_in.lines(Method::Get)).await) else {
        return Ok(None);
    };
    let Some(vms) = data_of::<Vec<ClusterVm>>(cx, vars, &answer)? else {
        tracing::warn!("Proxmox's answer holds no list of resources: the run's own state stands");
        return Ok(None);
    };
    Ok(Some(Listing { sign_in, vms }))
}

/// Whether `vm`, which its server's listing leaves out, is gone: `cluster/nextid?vmid=<id>`
/// answers the id only when no VM of the cluster holds it, which any signed-in user may ask and
/// which doesn't depend on what the credentials may audit. Absent then; any other answer (a VM
/// holds the id, the server refused or didn't answer) says nothing, traced: the run's own state
/// then stands.
async fn unlisted(cx: &Cx<'_>, vm: &ApiVm<'_>, sign_in: &SignIn) -> Option<crate::MachineState> {
    let url = format!("{}/api2/json/cluster/nextid?vmid={}", vm.base, vm.id);
    let answer = asked(request(cx, "status", (&url, Method::Get), vm.insecure, &sign_in.lines(Method::Get)).await)?;
    let free = answer.success() && answer::data::<FreeId>(&answer.stdout.join("\n")).ok().flatten().is_some_and(|f| f.is(&vm.id));
    if free {
        return Some(crate::MachineState::Absent);
    }
    let reason = cx.events.mask(&answer.reason());
    tracing::warn!(vm = %vm.id, %reason, "Proxmox doesn't list a VM it still holds the id of (credentials without VM.Audit on it?): the run's own state stands");
    None
}

/// A VM's state as the API's `status` names it.
fn reading(status: &str) -> crate::MachineState {
    match status {
        "running" => crate::MachineState::Running,
        "stopped" => crate::MachineState::Stopped,
        other => crate::MachineState::Unknown(other.to_owned()),
    }
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

impl ApiVm<'_> {
    /// Whether `other` is on the same server, signed in to as this one is: one listing answers
    /// for both.
    fn same_server(&self, other: &ApiVm<'_>) -> bool {
        (&self.base, self.insecure, self.sign_in) == (&other.base, other.insecure, other.sign_in)
    }
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
        let (base, insecure) = api_target(vars, (endpoint, insecure), at)?;
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
    let Some(answer) = asked(request(cx, "status", (&url, Method::Post), vm.insecure, &fields).await) else {
        return Ok(None);
    };
    Ok(data_of::<Ticket>(cx, vars, &answer)?.and_then(SignIn::ticket))
}

/// What `curl` answered a status read, when it ran to its end; a request that couldn't (`curl`
/// missing, the server silent past the limit) is traced, and the run's state then stands.
fn asked(done: Result<crate::process::Captured>) -> Option<crate::process::Captured> {
    done.map_err(|e| tracing::warn!(error = %e, "Proxmox's API couldn't be asked where the run's VMs stand: its state stands"))
        .ok()
}

/// The `data` of the API's `answer` ([`answer`]); none when it didn't answer one (unreachable,
/// not the API's), which is traced.
///
/// # Errors
///
/// `server_auth_failed` when it refused the credentials.
fn data_of<T: DeserializeOwned>(cx: &Cx<'_>, vars: &crate::vars::Vars, answer: &crate::process::Captured) -> Result<Option<T>> {
    if !answer.success() {
        let reason = cx.events.mask(&answer.reason());
        if crate::tool_output::sign_in::refused(&reason) {
            return Err(Error::ServerAuthFailed {
                step: "status".into(),
                server: crate::server_auth::server_of(vars.params()),
                reason,
            });
        }
        tracing::warn!(%reason, "Proxmox's API couldn't be asked where the run's VMs stand: its state stands");
        return Ok(None);
    }
    match answer::data(&answer.stdout.join("\n")) {
        Ok(data) => Ok(data),
        Err(e) => {
            tracing::warn!(error = %e, "Proxmox's API answered no JSON this runner reads: the run's state stands");
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

    fn curl(code: i32, stdout: &[&str]) -> crate::process::Captured {
        let lines = stdout.iter().map(|l| (*l).to_owned()).collect();
        crate::process::Captured::new("curl", crate::process::Exit::new(Some(code)), lines, Vec::new())
    }

    /// Regression (#133): `--fail-with-body` exists only from curl 7.76 on; the status is read
    /// from `--write-out` instead, and an error status fails the request as it did.
    #[test]
    fn the_http_status_is_read_after_the_body_on_any_curl() {
        assert_eq!(STATUS_LINE, "\\n%{http_code}", "curl writes the line break itself");
        let ok = http_answer(curl(0, &[r#"{"data":{"port":"5900"}}"#, "200"]));
        assert!(ok.success());
        assert_eq!(ok.stdout, [r#"{"data":{"port":"5900"}}"#]);
        let refused = http_answer(curl(0, &[r#"{"data":null}"#, "401"]));
        assert!(!refused.success());
        assert_eq!(refused.stdout, [r#"{"data":null}"#], "the body is kept");
        assert!(crate::tool_output::sign_in::refused(&refused.reason()), "{}", refused.reason());
        let failed = http_answer(curl(0, &["", "500"]));
        assert_eq!(failed.reason(), "curl: (22) The requested URL returned error: 500 (exited with code 22)");
        // curl itself failing (no server) is left as it said.
        let unreachable = http_answer(curl(7, &["000"]));
        assert_eq!(unreachable.exit.code, Some(7));
    }

    #[test]
    fn a_token_or_a_ticket_signs_the_websocket_in() {
        let t = SignIn::Token("root@pam!x=s3cr3t".into());
        assert_eq!(t.lines(Method::Post), ["header = \"Authorization: PVEAPIToken=root@pam!x=s3cr3t\""]);
        assert_eq!(t.header().0, "Authorization");
        let k = SignIn::Ticket {
            ticket: "PVE:t".into(),
            csrf: "c".into(),
        };
        assert_eq!(
            k.lines(Method::Post),
            ["cookie = \"PVEAuthCookie=PVE:t\"", "header = \"CSRFPreventionToken: c\""]
        );
        assert_eq!(k.header(), ("Cookie".to_owned(), SecretValue::new("PVEAuthCookie=PVE:t")));
    }
}
