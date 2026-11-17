//! A Proxmox VE server's API, over HTTPS whose certificate is trusted as the caller says
//! ([`Trust`]: the system's trust store, or a pinned SHA-256 for a self-signed one): sign in with
//! an API token or a user's password ([`sign_in`]), then read what a placement needs: the nodes,
//! each node's storages and bridges ([`placement`]), whether the ones chosen can take machines
//! ([`check`]), and a node's cores and memory ([`Session::capacity`]).
//!
//! A refused certificate ends the request before any credential is sent, and the error names the
//! certificate the server offered ([`SignInError::Certificate`]): what a caller asks a user to
//! confirm. Pins are the runner's [`isothrum_runner::tls`] fingerprints, so one saved elsewhere in
//! that format stays valid.

use std::time::Duration;

use isothrum_runner::tls::{Refusal, Tls, Trust};
use serde::de::DeserializeOwned;

use self::api::{Iface, Node, NodeStatus, Storage, Ticket};
use super::host_for_url;

pub mod api;
pub mod check;
pub mod placement;

/// The port Proxmox VE's API listens on by default.
pub const DEFAULT_PORT: u16 = 8006;

/// How long one request may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// A Proxmox VE server, as its API is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// Its name or address.
    pub host: String,
    /// Its API's port ([`DEFAULT_PORT`] unless changed).
    pub port: u16,
}

impl Server {
    /// The server at `host` on `port`.
    #[must_use]
    pub fn new(host: impl Into<String>, port: u16) -> Server {
        Server { host: host.into(), port }
    }

    /// The API's base URL, `https://<host>:<port>/api2/json`.
    #[must_use]
    pub fn api_url(&self) -> String {
        format!("https://{}:{}/api2/json", host_for_url(&self.host), self.port)
    }

    /// The endpoint Proxmox's Terraform providers take, `https://<host>:<port>/`.
    #[must_use]
    pub fn endpoint(&self) -> String {
        format!("https://{}:{}/", host_for_url(&self.host), self.port)
    }
}

/// What signs a session in.
#[derive(Clone, PartialEq, Eq)]
pub enum Credentials {
    /// An API token: its id (`user@realm!name`) and its secret.
    Token {
        /// `user@realm!name`.
        id: String,
        /// The token's secret.
        secret: String,
    },
    /// A user (`user@realm`) and their password, for a ticket.
    Password {
        /// `user@realm`.
        user: String,
        /// Their password.
        password: String,
    },
}

impl Credentials {
    /// A login as typed (a user or a token id, its realm `@pam` when it has none:
    /// [`with_default_realm`]) with its secret: a token when the login is a token id
    /// ([`is_token`]), else a user and their password.
    #[must_use]
    pub fn from_login(login: &str, secret: &str) -> Credentials {
        let login = with_default_realm(login);
        if is_token(&login) {
            Credentials::Token {
                id: login,
                secret: secret.to_owned(),
            }
        } else {
            Credentials::Password {
                user: login,
                password: secret.to_owned(),
            }
        }
    }

    /// The user they sign in as (a token's own user).
    #[must_use]
    pub fn user(&self) -> &str {
        match self {
            Credentials::Token { id, .. } => token_user(id),
            Credentials::Password { user, .. } => user,
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credentials::Token { id, .. } => f.debug_struct("Token").field("id", id).finish_non_exhaustive(),
            Credentials::Password { user, .. } => f.debug_struct("Password").field("user", user).finish_non_exhaustive(),
        }
    }
}

/// A login with its realm: `root` is `root@pam`, a token `root!name` is `root@pam!name`; one with
/// a realm (`admin@pve`, `root@pam!ci`) stays as typed.
#[must_use]
pub fn with_default_realm(login: &str) -> String {
    let (user, token) = login.split_once('!').map_or((login, None), |(u, t)| (u, Some(t)));
    let user = if user.is_empty() || user.contains('@') {
        user.to_owned()
    } else {
        format!("{user}@pam")
    };
    token.map_or_else(|| user.clone(), |t| format!("{user}!{t}"))
}

/// Whether `login` is an API token id (`user@realm!name`: a realm before the first `!`, a name
/// after it) rather than a user.
#[must_use]
pub fn is_token(login: &str) -> bool {
    login.split_once('!').is_some_and(|(user, name)| user.contains('@') && !name.is_empty())
}

/// The user a token belongs to (`root@pam!ci` is `root@pam`); a user is its own.
#[must_use]
pub fn token_user(login: &str) -> &str {
    login.split_once('!').map_or(login, |(u, _)| u)
}

/// The `api_token` value Proxmox's Terraform providers take, `<id>=<secret>`.
#[must_use]
pub fn terraform_token(id: &str, secret: &str) -> String {
    format!("{id}={secret}")
}

/// Why signing in failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SignInError {
    /// The server answered and refused the credentials.
    #[error("Proxmox VE refused the credentials")]
    Rejected,
    /// Its certificate isn't the one pinned (or none was pinned yet): nothing was sent to it.
    #[error("{0}")]
    Certificate(Refusal),
    /// The API couldn't be talked to (network, TLS, an answer that isn't the API's): why.
    #[error("{0}")]
    Failed(String),
}

/// Why a call to the API failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CallError {
    /// The API answered with this HTTP status, not a success (403: the user may not).
    #[error("HTTP {0}")]
    Status(u16),
    /// The server's certificate was refused (it changed since it was pinned).
    #[error("{0}")]
    Certificate(Refusal),
    /// There is no node by the name asked: the ones there are.
    #[error("there is no node \"{asked}\" (found: {})", found.join(", "))]
    NoSuchNode {
        /// The node asked for.
        asked: String,
        /// The nodes the user can see.
        found: Vec<String>,
    },
    /// The user can see no node at all.
    #[error("this user can't see any node")]
    NoNode,
    /// No answer that could be read (network, not the API's JSON): why.
    #[error("{0}")]
    Other(String),
}

/// A signed-in API client.
pub struct Session {
    http: reqwest::Client,
    tls: Tls,
    base: String,
    /// The header that signs each call in: a ticket's cookie, or the token.
    auth: (&'static str, String),
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("base", &self.base).finish_non_exhaustive()
    }
}

/// Signs in to `server` with `credentials`, its certificate trusted as `trust` says.
///
/// A token is proved by an authenticated call (`/version`); a password is exchanged for a ticket
/// (`access/ticket`).
///
/// # Errors
///
/// The credentials were refused, the certificate was ([`SignInError::Certificate`], before any
/// credential was sent), or the API couldn't be talked to.
pub async fn sign_in(server: &Server, credentials: &Credentials, trust: Trust) -> Result<Session, SignInError> {
    let tls = Tls::new(trust).map_err(|e| SignInError::Failed(e.to_string()))?;
    let http = reqwest::Client::builder()
        .tls_backend_preconfigured(tls.config().clone())
        .connect_timeout(REQUEST_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| SignInError::Failed(format!("no HTTP client: {}", chain(&e))))?;
    let base = server.api_url();
    match credentials {
        Credentials::Token { id, secret } => {
            let session = Session {
                http,
                tls,
                base,
                auth: ("Authorization", format!("PVEAPIToken={id}={secret}")),
            };
            match session.get("/version").await {
                Ok(_) => Ok(session),
                Err(CallError::Status(401)) => Err(SignInError::Rejected),
                Err(CallError::Certificate(refusal)) => Err(SignInError::Certificate(refusal)),
                Err(e) => Err(SignInError::Failed(e.to_string())),
            }
        }
        Credentials::Password { user, password } => {
            let sent = http
                .post(format!("{base}/access/ticket"))
                .form(&[("username", user.as_str()), ("password", password.as_str())])
                .send()
                .await;
            let res = match sent {
                Ok(res) => res,
                Err(e) => return Err(tls.refused().map_or_else(|| SignInError::Failed(chain(&e)), SignInError::Certificate)),
            };
            if res.status().as_u16() == 401 {
                return Err(SignInError::Rejected);
            }
            if !res.status().is_success() {
                return Err(SignInError::Failed(format!("the API answered HTTP {}", res.status().as_u16())));
            }
            let body = res.text().await.map_err(|e| SignInError::Failed(chain(&e)))?;
            let ticket = api::data::<Ticket>(&body)
                .map_err(|e| SignInError::Failed(format!("the sign-in's answer isn't the API's: {e}")))?
                .ticket
                .ok_or_else(|| SignInError::Failed("no ticket in the sign-in's answer".into()))?;
            Ok(Session {
                http,
                tls,
                base,
                auth: ("Cookie", format!("PVEAuthCookie={ticket}")),
            })
        }
    }
}

/// An error and its causes, on one line.
fn chain(e: &dyn std::error::Error) -> String {
    std::iter::successors(Some(e), |e| e.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(": ")
}

/// A node's cores and memory, as it reports them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capacity {
    /// Its logical CPUs.
    pub cores: u64,
    /// All its memory, in bytes.
    pub mem_total: u64,
    /// Its free memory, in bytes.
    pub mem_free: u64,
}

impl Capacity {
    /// A node's `/status` as its capacity; none when a figure is missing.
    #[must_use]
    pub fn of(status: &NodeStatus) -> Option<Capacity> {
        let (cpu, memory) = (status.cpuinfo.as_ref()?, status.memory.as_ref()?);
        Some(Capacity {
            cores: cpu.cpus?,
            mem_total: memory.total?,
            mem_free: memory.free?,
        })
    }
}

impl Session {
    /// GET `path` (under `/api2/json`): the answer's body, as the API wrote it.
    ///
    /// # Errors
    ///
    /// Not a success ([`CallError::Status`]), the certificate refused, or no answer.
    pub async fn get(&self, path: &str) -> Result<String, CallError> {
        let sent = self.http.get(format!("{}{path}", self.base)).header(self.auth.0, &self.auth.1).send().await;
        let res = match sent {
            Ok(res) => res,
            Err(e) => return Err(self.tls.refused().map_or_else(|| CallError::Other(chain(&e)), CallError::Certificate)),
        };
        if !res.status().is_success() {
            return Err(CallError::Status(res.status().as_u16()));
        }
        res.text().await.map_err(|e| CallError::Other(chain(&e)))
    }

    /// GET `path` and read its `data` as `T` ([`api::data`]).
    ///
    /// # Errors
    ///
    /// As [`Session::get`], or an answer that isn't the API's.
    pub async fn call<T: DeserializeOwned + Default>(&self, path: &str) -> Result<T, CallError> {
        read_data(&self.get(path).await?)
    }

    /// The nodes (`/nodes`).
    ///
    /// # Errors
    ///
    /// As [`Session::call`].
    pub async fn nodes(&self) -> Result<Vec<Node>, CallError> {
        self.call("/nodes").await
    }

    /// The node `asked` (when it is there), else the first one.
    ///
    /// # Errors
    ///
    /// As [`Session::call`]; there is no node `asked` ([`CallError::NoSuchNode`]), or none at all.
    pub async fn node(&self, asked: Option<&str>) -> Result<String, CallError> {
        pick_node(&self.nodes().await?, asked)
    }

    /// A node's storages (`/nodes/<node>/storage`).
    ///
    /// # Errors
    ///
    /// As [`Session::call`].
    pub async fn storages(&self, node: &str) -> Result<Vec<Storage>, CallError> {
        self.call(&format!("/nodes/{node}/storage")).await
    }

    /// A node's network interfaces (`/nodes/<node>/network`).
    ///
    /// # Errors
    ///
    /// As [`Session::call`].
    pub async fn network(&self, node: &str) -> Result<Vec<Iface>, CallError> {
        self.call(&format!("/nodes/{node}/network")).await
    }

    /// A node's bridges, Linux and Open vSwitch (`/nodes/<node>/network?type=any_bridge`).
    ///
    /// # Errors
    ///
    /// As [`Session::call`].
    pub async fn bridges(&self, node: &str) -> Result<Vec<Iface>, CallError> {
        self.call(&format!("/nodes/{node}/network?type=any_bridge")).await
    }

    /// A node's status (`/nodes/<node>/status`).
    ///
    /// # Errors
    ///
    /// As [`Session::call`].
    pub async fn status(&self, node: &str) -> Result<NodeStatus, CallError> {
        self.call(&format!("/nodes/{node}/status")).await
    }

    /// A node's cores and memory.
    ///
    /// # Errors
    ///
    /// As [`Session::status`]; a figure missing from it is [`CallError::Other`].
    pub async fn capacity(&self, node: &str) -> Result<Capacity, CallError> {
        Capacity::of(&self.status(node).await?).ok_or_else(|| CallError::Other(format!("the node {node} reported no cores or memory figures")))
    }
}

/// The `data` of an answer's `body`, read as `T`.
fn read_data<T: DeserializeOwned + Default>(body: &str) -> Result<T, CallError> {
    api::data(body).map_err(|e| CallError::Other(format!("the answer isn't the Proxmox VE API's: {e}")))
}

/// The node `asked` among `nodes`, else the first.
fn pick_node(nodes: &[Node], asked: Option<&str>) -> Result<String, CallError> {
    let names: Vec<&str> = nodes.iter().filter_map(|n| n.node.as_deref()).collect();
    match asked.filter(|n| !n.is_empty()) {
        Some(n) if names.contains(&n) => Ok(n.to_owned()),
        Some(n) => Err(CallError::NoSuchNode {
            asked: n.to_owned(),
            found: names.iter().map(|s| (*s).to_owned()).collect(),
        }),
        None => names.first().map(|s| (*s).to_owned()).ok_or(CallError::NoNode),
    }
}

#[cfg(test)]
mod tests;
