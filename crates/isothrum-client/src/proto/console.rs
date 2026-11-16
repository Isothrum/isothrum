//! A graphical console on the wire: where it answers, and how to sign in.

use std::fmt;
use std::net::SocketAddr;

use isothrum_runner::ConsoleProtocol;
use serde::{Deserialize, Serialize};

/// A machine's graphical console as the service reached it ([`isothrum_runner::Console`] on the
/// wire): where it answers, and how to sign in. Its secrets travel only over the owner-only
/// endpoint; its debug form never shows one.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ReachedConsole {
    /// The machine.
    pub machine: String,
    /// The protocol it speaks.
    pub protocol: ConsoleProtocol,
    /// Where it answers.
    pub endpoint: ConsoleEndpoint,
    /// The account to sign in as, when it signs in to the machine's own OS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Its password. Never print or log it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// What to know of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Whether an SSH tunnel of the service carries it: open while the connection that asked is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub tunnelled: bool,
}

/// Where a graphical console answers ([`isothrum_runner::Endpoint`] on the wire).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsoleEndpoint {
    /// A TCP port of the loopback, speaking the console's protocol.
    Loopback {
        /// The address.
        address: SocketAddr,
    },
    /// Proxmox VE's VNC websocket: connect to `websocket` with the header `header: authorization`
    /// (its certificate unverified when `insecure`), then speak VNC with `ticket` as the password.
    ProxmoxVnc {
        /// The websocket's URL (it holds the ticket).
        websocket: String,
        /// The VNC port opened on the node.
        port: u16,
        /// The ticket.
        ticket: String,
        /// The signing-in header's name.
        header: String,
        /// Its value.
        authorization: String,
        /// The server's certificate isn't verified.
        insecure: bool,
    },
}

impl ReachedConsole {
    /// `console` on the wire; none for an endpoint this protocol doesn't carry.
    #[must_use]
    pub fn of(console: &isothrum_runner::Console) -> Option<ReachedConsole> {
        let endpoint = match &console.endpoint {
            isothrum_runner::Endpoint::Loopback(address) => ConsoleEndpoint::Loopback { address: *address },
            isothrum_runner::Endpoint::ProxmoxVnc(vnc) => ConsoleEndpoint::ProxmoxVnc {
                websocket: vnc.websocket.expose().to_owned(),
                port: vnc.port,
                ticket: vnc.ticket.expose().to_owned(),
                header: vnc.authorization.0.clone(),
                authorization: vnc.authorization.1.expose().to_owned(),
                insecure: vnc.insecure,
            },
            _ => return None,
        };
        Some(ReachedConsole {
            machine: console.machine.clone(),
            protocol: console.protocol,
            endpoint,
            user: console.user.clone(),
            password: console.password.as_ref().map(|p| p.expose().to_owned()),
            note: console.note.clone(),
            tunnelled: console.tunnel.is_some(),
        })
    }
}

impl fmt::Debug for ReachedConsole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReachedConsole")
            .field("machine", &self.machine)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("note", &self.note)
            .field("tunnelled", &self.tunnelled)
            .finish()
    }
}

impl fmt::Debug for ConsoleEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConsoleEndpoint::Loopback { address } => f.debug_struct("Loopback").field("address", address).finish(),
            ConsoleEndpoint::ProxmoxVnc { port, header, insecure, .. } => f
                .debug_struct("ProxmoxVnc")
                .field("port", port)
                .field("header", header)
                .field("insecure", insecure)
                .finish_non_exhaustive(),
        }
    }
}
