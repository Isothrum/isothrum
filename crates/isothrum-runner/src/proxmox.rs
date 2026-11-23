//! The Proxmox VE API client: signing in (an API token as it is, a user's password traded for a
//! ticket), `vncproxy`, which opens a VM's VNC websocket for one session, and the cluster's VMs as
//! the server has them (`cluster/resources`, `cluster/nextid`). The server's certificate is
//! checked as [`Trust`] says: by the system's trust store, or, for a self-signed one, by its
//! SHA-256 fingerprint alone ([`crate::tls`]); a certificate refused ends the request before
//! anything (a token, a password) is sent. Every answer is `{"data": …}`, read here into the
//! fields the runner uses; unknown fields are skipped.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::secrets::SecretValue;
use crate::tls::{Refusal, Tls, Trust};

/// How long one request may take, connecting included.
const REQUEST_LIMIT: Duration = Duration::from_secs(25);

/// The server requests go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Api {
    /// `https://<server>[:<port>]`, without a path.
    pub(crate) base: String,
    /// How its certificate is trusted.
    pub(crate) trust: Trust,
}

impl Api {
    /// The API at `endpoint` (`https://<server>:8006`, with or without the provider's
    /// `/api2/json` and a last `/`), its certificate trusted as `trust` says; none when it isn't
    /// an `https` server.
    pub(crate) fn at(endpoint: &str, trust: Trust) -> Option<Api> {
        let base = endpoint.trim().trim_end_matches('/');
        let base = base.strip_suffix("/api2/json").unwrap_or(base).trim_end_matches('/');
        let server = base.strip_prefix("https://")?;
        let (host, port) = server.rsplit_once(':').map_or((server, None), |(h, p)| (h, Some(p)));
        let fine = crate::graphical::native::host_name(host) && port.is_none_or(|p| p.parse::<u16>().is_ok());
        fine.then(|| Api { base: base.to_owned(), trust })
    }

    /// The server, as a `wss://` URL names it.
    pub(crate) fn server(&self) -> &str {
        self.base.strip_prefix("https://").unwrap_or(&self.base)
    }

    /// A client for this server's requests.
    pub(crate) fn client(&self) -> Result<Client, Failure> {
        let tls = Tls::new(self.trust).map_err(|e| Failure::Other(e.to_string()))?;
        let http = reqwest::Client::builder()
            .tls_backend_preconfigured(tls.config().clone())
            .connect_timeout(REQUEST_LIMIT)
            .timeout(REQUEST_LIMIT)
            .build()
            .map_err(|e| Failure::Other(format!("no HTTP client: {}", chain(&e))))?;
        Ok(Client { api: self.clone(), http, tls })
    }
}

/// An HTTP client for one server's requests, and the certificate it was last offered.
#[derive(Debug)]
pub(crate) struct Client {
    api: Api,
    http: reqwest::Client,
    tls: Tls,
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

/// How the API is signed in to, once [`Client::sign_in`] did.
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
    /// The headers that sign a request in (a GET needs no CSRF token, and ignores it).
    fn headers(&self) -> Vec<(&'static str, String)> {
        match self {
            SignIn::Token(t) => vec![("Authorization", format!("PVEAPIToken={}", t.expose()))],
            SignIn::Ticket { ticket, csrf } => vec![
                ("Cookie", format!("PVEAuthCookie={}", ticket.expose())),
                ("CSRFPreventionToken", csrf.expose().to_owned()),
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
    /// The server's certificate isn't the one pinned for it: nothing was sent.
    Untrusted(Refusal),
    /// Another HTTP status: the API refused the request itself.
    Status(u16),
    /// Any other failure (the connection, the system's trust store refusing the certificate, an
    /// answer without what was asked): its reason.
    Other(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::Refused(status) => write!(f, "the server refused the credentials (HTTP {status})"),
            Failure::Untrusted(refusal) => refusal.fmt(f),
            Failure::Status(status) => write!(f, "the API answered HTTP {status}"),
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
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum NumberOrText {
    Number(u64),
    Text(String),
}

impl NumberOrText {
    /// Whether it writes `id`.
    fn is(&self, id: &str) -> bool {
        match self {
            NumberOrText::Number(n) => n.to_string() == id,
            NumberOrText::Text(t) => t == id,
        }
    }

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

/// The VNC websocket `vncproxy` opened: its port and ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Vnc {
    /// The port, on the node.
    pub(crate) port: u16,
    /// The ticket: the VNC password.
    pub(crate) ticket: SecretValue,
}

/// One VM of the cluster, as `cluster/resources` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct ClusterVm {
    /// Its id.
    vmid: NumberOrText,
    /// `running`, `stopped`, …
    pub(crate) status: Option<String>,
}

impl ClusterVm {
    /// Whether it is VM `id`.
    pub(crate) fn is(&self, id: &str) -> bool {
        self.vmid.is(id)
    }
}

/// The API path of VM `vm_id` on `node` (`/api2/json/nodes/<node>/qemu/<id>`).
pub(crate) fn vm_path(node: &str, vm_id: &str) -> String {
    format!("/api2/json/nodes/{node}/qemu/{vm_id}")
}

impl Client {
    /// How the API is signed in to with `credentials`: a token as it is, a password traded for a
    /// ticket (`access/ticket`).
    pub(crate) async fn sign_in(&self, credentials: &Credentials) -> Result<SignIn, Failure> {
        match credentials {
            Credentials::Token(t) => Ok(SignIn::Token(t.clone())),
            Credentials::Password { user, password } => {
                let url = format!("{}/api2/json/access/ticket", self.api.base);
                let form = [("username", user.as_str()), ("password", password.expose())];
                let said: Ticket = self.request(self.http.post(url).form(&form)).await?;
                match (said.ticket, said.csrf) {
                    (Some(ticket), Some(csrf)) => Ok(SignIn::Ticket {
                        ticket: SecretValue::new(ticket),
                        csrf: SecretValue::new(csrf),
                    }),
                    _ => Err(Failure::Other("an `access/ticket` answer without its ticket".to_owned())),
                }
            }
        }
    }

    /// `vncproxy` of VM `vm_id` on `node`, for a websocket: its port and ticket.
    pub(crate) async fn vncproxy(&self, sign_in: &SignIn, (node, vm_id): (&str, &str)) -> Result<Vnc, Failure> {
        let url = format!("{}{}/vncproxy", self.api.base, vm_path(node, vm_id));
        let said: VncProxy = self.request(signed(self.http.post(url), sign_in).form(&[("websocket", "1")])).await?;
        match (said.port.as_ref().and_then(NumberOrText::as_u16), said.ticket) {
            (Some(port), Some(ticket)) => Ok(Vnc {
                port,
                ticket: SecretValue::new(ticket),
            }),
            _ => Err(Failure::Other("a `vncproxy` answer without its port and ticket".to_owned())),
        }
    }

    /// The VMs of the cluster the sign-in may see (`cluster/resources?type=vm`), wherever each
    /// migrated.
    pub(crate) async fn cluster_vms(&self, sign_in: &SignIn) -> Result<Vec<ClusterVm>, Failure> {
        let url = format!("{}/api2/json/cluster/resources?type=vm", self.api.base);
        self.request(signed(self.http.get(url), sign_in)).await
    }

    /// Whether no VM of the cluster holds id `vm_id` (`cluster/nextid?vmid=<id>` answers the id
    /// only then; any signed-in user may ask, whatever the sign-in may see). A listing leaves out
    /// the VMs the sign-in may not audit: only this tells a VM gone from one hidden.
    pub(crate) async fn id_free(&self, sign_in: &SignIn, vm_id: &str) -> Result<bool, Failure> {
        let url = format!("{}/api2/json/cluster/nextid?vmid={vm_id}", self.api.base);
        match self.request::<NumberOrText>(signed(self.http.get(url), sign_in)).await {
            Ok(id) => Ok(id.is(vm_id)),
            // A VM holds the id: the API refuses it as a request it can't take.
            Err(Failure::Status(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The `data` of the answer to `request`.
    async fn request<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T, Failure> {
        let answer = match request.send().await {
            Ok(answer) => answer,
            Err(e) => return Err(self.tls.refused().map_or_else(|| Failure::Other(chain(&e)), Failure::Untrusted)),
        };
        let status = answer.status().as_u16();
        let body = answer
            .text()
            .await
            .map_err(|e| Failure::Other(format!("the answer couldn't be read: {}", chain(&e))))?;
        data(status, &body)
    }
}

/// `request`, signed in as `sign_in` says.
fn signed(request: reqwest::RequestBuilder, sign_in: &SignIn) -> reqwest::RequestBuilder {
    sign_in.headers().into_iter().fold(request, |r, (name, value)| r.header(name, value))
}

/// The `data` of an answer of HTTP `status` with `body`: a refused sign-in and another refusal
/// told apart.
fn data<T: DeserializeOwned>(status: u16, body: &str) -> Result<T, Failure> {
    match status {
        401 | 403 => return Err(Failure::Refused(status)),
        code if code >= 400 => return Err(Failure::Status(code)),
        _ => {}
    }
    match serde_json::from_str::<Answer<T>>(body).map(|a| a.data) {
        Ok(Some(d)) => Ok(d),
        Ok(None) => Err(Failure::Other("an answer with no data".to_owned())),
        Err(e) => Err(Failure::Other(format!("an answer that isn't the API's JSON: {e}"))),
    }
}

/// An error with its causes, on one line.
fn chain(e: &dyn std::error::Error) -> String {
    std::iter::successors(Some(e), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::https::{Server, fingerprint};
    use crate::tls::Fingerprint;

    #[test]
    fn an_endpoint_is_an_https_server_with_or_without_its_path() {
        let api = |e: &str| Api::at(e, Trust::System).map(|a| a.base);
        assert_eq!(api("https://pve.stack:8006/api2/json").as_deref(), Some("https://pve.stack:8006"));
        assert_eq!(api("https://10.0.0.5:8006/").as_deref(), Some("https://10.0.0.5:8006"));
        assert_eq!(api("https://pve").as_deref(), Some("https://pve"));
        for bad in ["http://pve:8006", "https://user@pve:8006", "https://pve:x", "https://pve/a/b", ""] {
            assert_eq!(api(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_sign_in_signs_a_request_and_the_websocket() {
        let t = SignIn::Token(SecretValue::new("root@pam!x=s3cr3t"));
        assert_eq!(t.headers(), [("Authorization", "PVEAPIToken=root@pam!x=s3cr3t".to_owned())]);
        assert_eq!(t.header().0, "Authorization");
        let k = SignIn::Ticket {
            ticket: SecretValue::new("PVE:t"),
            csrf: SecretValue::new("c"),
        };
        assert_eq!(
            k.headers(),
            [("Cookie", "PVEAuthCookie=PVE:t".to_owned()), ("CSRFPreventionToken", "c".to_owned())]
        );
        assert_eq!(k.header(), ("Cookie".to_owned(), SecretValue::new("PVEAuthCookie=PVE:t")));
    }

    #[test]
    fn an_answer_gives_its_data_and_a_refusal_is_told_apart() {
        let vnc: VncProxy = data(200, r#"{"data":{"port":"5900","ticket":"PVEVNC:x"}}"#).unwrap();
        assert_eq!(
            (vnc.port.as_ref().and_then(NumberOrText::as_u16), vnc.ticket.as_deref()),
            (Some(5900), Some("PVEVNC:x"))
        );
        assert_eq!(data::<VncProxy>(401, r#"{"data":null}"#).unwrap_err(), Failure::Refused(401));
        assert_eq!(data::<VncProxy>(403, "").unwrap_err(), Failure::Refused(403));
        assert_eq!(data::<NumberOrText>(400, r#"{"errors":{"vmid":"taken"}}"#).unwrap_err(), Failure::Status(400));
        assert!(matches!(data::<VncProxy>(200, r#"{"data":null}"#), Err(Failure::Other(_))));
        assert!(matches!(data::<VncProxy>(200, "<html>"), Err(Failure::Other(_))));
        let vms: Vec<ClusterVm> = data(200, r#"{"data":[{"vmid":101,"status":"running","extra":1},{"vmid":"102"}]}"#).unwrap();
        assert!(vms[0].is("101") && vms[1].is("102") && vms[1].status.is_none());
    }

    /// A password sign-in, then `vncproxy` with the ticket, against a server whose certificate
    /// is pinned.
    #[tokio::test]
    async fn a_pinned_server_signs_a_password_in_and_opens_a_vnc_websocket() {
        let server = Server::start(|r| match (r.method.as_str(), r.target.as_str()) {
            ("POST", "/api2/json/access/ticket") => {
                assert_eq!(r.body, "username=root%40pam&password=p%26ss", "the form, encoded");
                (200, r#"{"data":{"ticket":"PVE:t","CSRFPreventionToken":"c"}}"#.to_owned())
            }
            ("POST", "/api2/json/nodes/pve/qemu/101/vncproxy") => {
                assert_eq!((r.header("cookie"), r.header("csrfpreventiontoken")), (Some("PVEAuthCookie=PVE:t"), Some("c")));
                assert_eq!(r.body, "websocket=1");
                (200, r#"{"data":{"port":"5900","ticket":"PVEVNC:x"}}"#.to_owned())
            }
            _ => panic!("an unscripted request: {r:?}"),
        })
        .await
        .unwrap();
        let client = Api::at(&server.endpoint, Trust::Pinned(Some(fingerprint()))).unwrap().client().unwrap();
        let password = Credentials::Password {
            user: "root@pam".into(),
            password: SecretValue::new("p&ss"),
        };
        let sign_in = client.sign_in(&password).await.unwrap();
        let vnc = client.vncproxy(&sign_in, ("pve", "101")).await.unwrap();
        assert_eq!((vnc.port, vnc.ticket.expose()), (5900, "PVEVNC:x"));
    }

    /// Another certificate than the pinned one, or a self-signed one the system's trust store
    /// checks: refused before any request, the one offered told.
    #[tokio::test]
    async fn a_certificate_not_pinned_is_refused_before_anything_is_sent() {
        let server = Server::start(|_| (200, r#"{"data":{"ticket":"t","CSRFPreventionToken":"c"}}"#.to_owned()))
            .await
            .unwrap();
        let password = Credentials::Password {
            user: "root@pam".into(),
            password: SecretValue::new("pw"),
        };
        let other = Fingerprint::of(b"another certificate");
        let refused = Api::at(&server.endpoint, Trust::Pinned(Some(other)))
            .unwrap()
            .client()
            .unwrap()
            .sign_in(&password)
            .await;
        let changed = Refusal::Changed {
            pinned: other,
            presented: fingerprint(),
        };
        assert_eq!(refused, Err(Failure::Untrusted(changed)));
        let system = Api::at(&server.endpoint, Trust::System).unwrap().client().unwrap().sign_in(&password).await;
        assert!(matches!(system, Err(Failure::Other(_))), "{system:?}");
        assert_eq!(server.requests().len(), 0, "no request reached it");
    }
}
