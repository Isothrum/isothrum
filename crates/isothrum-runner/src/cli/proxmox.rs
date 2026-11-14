//! The Proxmox VE API adapter, through `curl`: signing in (an API token as it is, a user's
//! password traded for a ticket) and `vncproxy`, which opens a VM's VNC websocket for one session.
//! The credentials reach `curl` in a config file written owner-only in the folder the caller names
//! (the run's state folder), one per request, piped into it and removed however the request ends:
//! never on its command line. Every answer is `{"data": …}`, read here into the fields the runner
//! uses; unknown fields are skipped.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::{Call, CliError};
use crate::secrets::SecretValue;

/// The tool.
const TOOL: &str = "curl";

/// What `--write-out` prints after the body: the HTTP status on a line of its own (curl writes
/// the line break itself). `--fail-with-body` would say it, but only from curl 7.76 on.
const STATUS_LINE: &str = "\\n%{http_code}";

/// The server one request goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Api {
    /// `https://<server>[:<port>]`, without a path.
    pub(crate) base: String,
    /// The server's certificate isn't verified.
    pub(crate) insecure: bool,
}

impl Api {
    /// The API at `endpoint` (`https://<server>:8006`, with or without the provider's
    /// `/api2/json` and a last `/`); none when it isn't an `https` server.
    pub(crate) fn at(endpoint: &str, insecure: bool) -> Option<Api> {
        let base = endpoint.trim().trim_end_matches('/');
        let base = base.strip_suffix("/api2/json").unwrap_or(base).trim_end_matches('/');
        let server = base.strip_prefix("https://")?;
        let (host, port) = server.rsplit_once(':').map_or((server, None), |(h, p)| (h, Some(p)));
        let fine = crate::graphical::native::host_name(host) && port.is_none_or(|p| p.parse::<u16>().is_ok());
        fine.then(|| Api {
            base: base.to_owned(),
            insecure,
        })
    }

    /// The server, as a `wss://` URL names it.
    pub(crate) fn server(&self) -> &str {
        self.base.strip_prefix("https://").unwrap_or(&self.base)
    }
}

/// What signs a request in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Credentials {
    /// An API token, `<user>@<realm>!<id>=<secret>`.
    Token(SecretValue),
    /// A user and their password.
    Password {
        /// `<user>@<realm>`.
        user: String,
        /// The password.
        password: SecretValue,
    },
}

/// How the API is signed in to, once [`sign_in`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SignIn {
    /// An API token: `Authorization: PVEAPIToken=<token>`.
    Token(SecretValue),
    /// A ticket from `access/ticket`: the cookie, and the CSRF header a POST needs.
    Ticket {
        /// The ticket.
        ticket: SecretValue,
        /// The CSRF token.
        csrf: SecretValue,
    },
}

impl SignIn {
    /// The config lines that sign a POST in.
    fn lines(&self) -> Vec<String> {
        match self {
            SignIn::Token(t) => vec![format!("header = {}", quoted(&format!("Authorization: PVEAPIToken={}", t.expose())))],
            SignIn::Ticket { ticket, csrf } => vec![
                format!("cookie = {}", quoted(&format!("PVEAuthCookie={}", ticket.expose()))),
                format!("header = {}", quoted(&format!("CSRFPreventionToken: {}", csrf.expose()))),
            ],
        }
    }

    /// The header that signs a websocket in: its name and value.
    pub(crate) fn header(&self) -> (String, SecretValue) {
        match self {
            SignIn::Token(t) => ("Authorization".into(), SecretValue::new(format!("PVEAPIToken={}", t.expose()))),
            SignIn::Ticket { ticket, .. } => ("Cookie".into(), SecretValue::new(format!("PVEAuthCookie={}", ticket.expose()))),
        }
    }
}

/// A failed API request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The server refused the credentials (401, 403).
    Refused(u16),
    /// Any other failure (curl's own, another HTTP status, an answer without what was asked):
    /// its reason.
    Other(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(status) => write!(f, "the server refused the credentials (HTTP {status})"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// `access/ticket`'s answer to a user's password.
#[derive(Debug, Deserialize)]
struct Ticket {
    ticket: Option<String>,
    #[serde(rename = "CSRFPreventionToken")]
    csrf: Option<String>,
}

/// `vncproxy`'s answer: the port of the VM's VNC websocket and the ticket that opens it.
#[derive(Debug, Deserialize)]
struct VncProxy {
    port: Option<NumberOrText>,
    ticket: Option<String>,
}

/// A number the API writes as a number or as a string, depending on the call and the version.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum NumberOrText {
    Number(u64),
    Text(String),
}

impl NumberOrText {
    fn as_u16(&self) -> Option<u16> {
        match self {
            NumberOrText::Number(n) => u16::try_from(*n).ok(),
            NumberOrText::Text(s) => s.parse().ok(),
        }
    }
}

/// The API's envelope.
#[derive(Debug, Deserialize)]
struct Answer<T> {
    data: Option<T>,
}

/// The `data` of `body`; none when it has none.
fn data<T: DeserializeOwned>(body: &str) -> Result<Option<T>, serde_json::Error> {
    serde_json::from_str::<Answer<T>>(body).map(|a| a.data)
}

/// The VNC websocket `vncproxy` opened: its port and ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Vnc {
    /// The port, on the node.
    pub(crate) port: u16,
    /// The ticket: the VNC password.
    pub(crate) ticket: SecretValue,
}

/// How the API is signed in to with `credentials`: a token as it is, a password traded for a
/// ticket (`access/ticket`). `scratch` is where the request's curl config waits.
pub(crate) async fn sign_in(call: &Call<'_>, api: &Api, credentials: &Credentials, scratch: &Path) -> Result<SignIn, CliError<Failure>> {
    match credentials {
        Credentials::Token(t) => Ok(SignIn::Token(t.clone())),
        Credentials::Password { user, password } => {
            let fields = [
                format!("data-urlencode = {}", quoted(&format!("username={user}"))),
                format!("data-urlencode = {}", quoted(&format!("password={}", password.expose()))),
            ];
            let said: Ticket = post(call, &format!("{}/api2/json/access/ticket", api.base), api.insecure, &fields, scratch).await?;
            match (said.ticket, said.csrf) {
                (Some(ticket), Some(csrf)) => Ok(SignIn::Ticket {
                    ticket: SecretValue::new(ticket),
                    csrf: SecretValue::new(csrf),
                }),
                _ => Err(unexpected("an `access/ticket` answer without its ticket")),
            }
        }
    }
}

/// `vncproxy` of VM `vm_id` on `node`, for a websocket: its port and ticket.
pub(crate) async fn vncproxy(call: &Call<'_>, api: &Api, sign_in: &SignIn, (node, vm_id): (&str, &str), scratch: &Path) -> Result<Vnc, CliError<Failure>> {
    let mut fields = sign_in.lines();
    fields.push("data = \"websocket=1\"".to_owned());
    let url = format!("{}{}/vncproxy", api.base, vm_path(node, vm_id));
    let said: VncProxy = post(call, &url, api.insecure, &fields, scratch).await?;
    match (said.port.as_ref().and_then(NumberOrText::as_u16), said.ticket) {
        (Some(port), Some(ticket)) => Ok(Vnc {
            port,
            ticket: SecretValue::new(ticket),
        }),
        _ => Err(unexpected("a `vncproxy` answer without its port and ticket")),
    }
}

/// The API path of VM `vm_id` on `node` (`/api2/json/nodes/<node>/qemu/<id>`).
pub(crate) fn vm_path(node: &str, vm_id: &str) -> String {
    format!("/api2/json/nodes/{node}/qemu/{vm_id}")
}

/// [`CliError::Unexpected`] saying `what`.
fn unexpected(what: &str) -> CliError<Failure> {
    CliError::Unexpected {
        tool: TOOL,
        what: what.to_owned(),
    }
}

/// The `data` of a POST to `url`, `fields` (its sign-in and form) in the config file curl reads.
async fn post<T: DeserializeOwned>(call: &Call<'_>, url: &str, insecure: bool, fields: &[String], scratch: &Path) -> Result<T, CliError<Failure>> {
    let mut text = format!("url = {}\nrequest = \"POST\"\n", quoted(url));
    for f in fields {
        text.push_str(f);
        text.push('\n');
    }
    let config = Config::write(scratch, &text)?;
    let mut args: Vec<String> = ["--silent", "--show-error", "--write-out", STATUS_LINE, "--max-time", "25", "--config", "-"]
        .map(str::to_owned)
        .to_vec();
    if insecure {
        args.push("--insecure".to_owned());
    }
    let out = call.run_fed::<Failure>(TOOL, args, &config.path).await;
    drop(config);
    let mut out = out?;
    let status = out.stdout.pop().and_then(|line| line.trim().parse::<u16>().ok());
    match status {
        Some(code @ (401 | 403)) => return Err(failed(Failure::Refused(code))),
        Some(code) if code >= 400 => return Err(failed(Failure::Other(format!("the API answered HTTP {code}")))),
        _ if !out.success() => return Err(failed(Failure::Other(out.reason()))),
        _ => {}
    }
    match data(&out.stdout.join("\n")) {
        Ok(Some(d)) => Ok(d),
        Ok(None) => Err(unexpected("an answer with no data")),
        Err(e) => Err(unexpected(&format!("an answer that isn't the API's JSON: {e}"))),
    }
}

/// [`CliError::Failed`] of `failure`.
fn failed(failure: Failure) -> CliError<Failure> {
    CliError::Failed { tool: TOOL, failure }
}

/// One request's curl config, owner-only, removed when dropped (once curl ran, or when the request
/// is cut short).
struct Config {
    path: PathBuf,
}

impl Config {
    /// `text` in a new file of its own in `dir`.
    fn write(dir: &Path, text: &str) -> Result<Config, CliError<Failure>> {
        let suffix = crate::encoding::random_hex::<8>().map_err(|e| failed(Failure::Other(format!("no random name for a request: {e}"))))?;
        let path = dir.join(format!("proxmox-{suffix}.curl"));
        crate::store::write_private(&path, text.as_bytes()).map_err(|e| failed(Failure::Other(format!("{} couldn't be written: {e}", path.display()))))?;
        Ok(Config { path })
    }
}

impl Drop for Config {
    fn drop(&mut self) {
        if let Err(e) = crate::store::remove_path(&self.path) {
            tracing::error!(error = %e, path = %self.path.display(), "a Proxmox request's curl config was left in the run's state folder");
        }
    }
}

/// `s` as a double-quoted value of a curl config file (`\`, `"` and line breaks escaped).
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::testing;
    use crate::process::fake::{FakeProcesses, Reply};

    #[test]
    fn an_endpoint_is_an_https_server_with_or_without_its_path() {
        let api = |e: &str| Api::at(e, false).map(|a| a.base);
        assert_eq!(api("https://pve.lab:8006/api2/json").as_deref(), Some("https://pve.lab:8006"));
        assert_eq!(api("https://10.0.0.5:8006/").as_deref(), Some("https://10.0.0.5:8006"));
        assert_eq!(api("https://pve").as_deref(), Some("https://pve"));
        for bad in ["http://pve:8006", "https://user@pve:8006", "https://pve:x", "https://pve/a/b", ""] {
            assert_eq!(api(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_value_is_quoted_for_curl_and_a_sign_in_signs_the_websocket() {
        assert_eq!(quoted(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quoted("x\ny"), "\"x\\ny\"", "a new line can't start another option");
        let t = SignIn::Token(SecretValue::new("root@pam!x=s3cr3t"));
        assert_eq!(t.lines(), ["header = \"Authorization: PVEAPIToken=root@pam!x=s3cr3t\""]);
        assert_eq!(t.header().0, "Authorization");
        let k = SignIn::Ticket {
            ticket: SecretValue::new("PVE:t"),
            csrf: SecretValue::new("c"),
        };
        assert_eq!(k.lines(), ["cookie = \"PVEAuthCookie=PVE:t\"", "header = \"CSRFPreventionToken: c\""]);
        assert_eq!(k.header(), ("Cookie".to_owned(), SecretValue::new("PVEAuthCookie=PVE:t")));
    }

    /// `vncproxy` with the fake answering `reply`, in `dir`.
    async fn vncproxy_answered(reply: Reply, dir: &Path) -> Result<Vnc, CliError<Failure>> {
        let args = format!("curl --silent --show-error --write-out {STATUS_LINE} --max-time 25 --config -");
        let fake = FakeProcesses::script().expect(&args, reply).build();
        let call = testing::call(&fake);
        let api = Api::at("https://pve:8006", false).unwrap();
        let token = SignIn::Token(SecretValue::new("t"));
        vncproxy(&call, &api, &token, ("pve", "101"), dir).await
    }

    #[tokio::test]
    async fn vncproxy_gives_its_port_and_ticket() {
        let dir = tempfile::tempdir().unwrap();
        let reply = Reply::ok("{\"data\":{\"port\":\"5900\",\"ticket\":\"PVEVNC:x\"}}\n200");
        let vnc = vncproxy_answered(reply, dir.path()).await.unwrap();
        assert_eq!((vnc.port, vnc.ticket.expose()), (5900, "PVEVNC:x"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no request's config is left");
    }

    #[tokio::test]
    async fn a_refused_vncproxy_is_told_apart() {
        let dir = tempfile::tempdir().unwrap();
        let refused = vncproxy_answered(Reply::ok("{\"data\":null}\n401"), dir.path()).await.unwrap_err();
        assert!(
            matches!(
                refused,
                CliError::Failed {
                    failure: Failure::Refused(401),
                    ..
                }
            ),
            "{refused}"
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "no request's config is left");
    }
}
