//! A machine's graphical console (its screen, keyboard and mouse), found where its hypervisor
//! keeps it, asked through the run's format driver ([`crate::formats::FormatDriver::graphical`]):
//! [`ConsoleOffer`], what a status says of each machine's, and [`Console`], what
//! [`crate::Running::graphical_console`] reaches.
//!
//! The runner guarantees where a console answers: a TCP port of this computer's loopback
//! ([`Endpoint::Loopback`]), refusing one the hypervisor says listens anywhere else and one that
//! doesn't answer, or, for Proxmox VE, its API's own VNC websocket with a ticket for this session
//! ([`Endpoint::ProxmoxVnc`]).
//!
//! - **VirtualBox** (Vagrant): its remote display (RDP), as `VBoxManage showvminfo` says of the
//!   VM Vagrant records.
//! - **libvirt** (Vagrant): `virsh domdisplay` of the domain Vagrant records.
//! - **QEMU** (vagrant-qemu): `info vnc` on the VM's monitor socket.
//! - **Proxmox VE** (Terraform): `vncproxy` over its API, the VM's node and id from the module's
//!   state.
//! - **A module's Windows VM** (Terraform): its Remote Desktop (or a VNC server the module names),
//!   which answers only on the module's own networks, through an SSH tunnel from this computer's
//!   loopback to the Linux machine beside it ([`tunnel`]): a [`Tunnel`] the [`Console`] owns.
//!
//! The other hypervisors give no screen the runner reaches: their VMs open in the hypervisor's
//! own app ([`native`]).

pub(crate) mod native;
pub(crate) mod tunnel;

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::error::Error;
use crate::secrets::SecretValue;

pub use native::{NativeApp, NativeConsole, NativeNeed, NativeOpen};
pub use tunnel::Tunnel;

/// The protocol a graphical console speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConsoleProtocol {
    /// VNC (RFB).
    Vnc,
    /// RDP (VirtualBox's remote display).
    Rdp,
    /// None the runner reaches.
    None,
}

/// What a status says of a machine's graphical console: its protocol, or `none` and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ConsoleOffer {
    /// `vnc`, `rdp`, or `none`.
    pub protocol: ConsoleProtocol,
    /// Why there is none, or what it needs (an extension pack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Why it can't be shown now, as the status found it on a running machine: its hypervisor
    /// gives it no display (a VM started without one). None when it can, or wasn't looked at: the
    /// machine doesn't run, it has none, or it is reached through Proxmox's API, which a status
    /// doesn't open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl ConsoleOffer {
    /// A console of `protocol`, saying `note`.
    pub(crate) fn of(protocol: ConsoleProtocol, note: Option<&str>) -> ConsoleOffer {
        ConsoleOffer {
            protocol,
            note: note.map(str::to_owned),
            unavailable: None,
        }
    }

    /// No console the runner reaches, saying why.
    pub(crate) fn none(why: &str) -> ConsoleOffer {
        ConsoleOffer::of(ConsoleProtocol::None, Some(why))
    }

    /// This offer, unavailable now for the reason `why` gives (none: it is).
    pub(crate) fn unavailable(mut self, why: Option<String>) -> ConsoleOffer {
        self.unavailable = why;
        self
    }
}

/// Where a graphical console answers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Endpoint {
    /// A TCP port of this computer's loopback speaking the console's protocol (VNC, RDP).
    Loopback(SocketAddr),
    /// Proxmox VE's VNC websocket, through its API.
    ProxmoxVnc(ProxmoxVnc),
}

/// Proxmox VE's VNC websocket for one VM, opened for one session: connect to `websocket` with
/// the `authorization` header (TLS unverified when `insecure`), then speak VNC with `ticket` as
/// the password. Every secret is a [`SecretValue`]: never print or log one.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxmoxVnc {
    /// `wss://<server>/api2/json/nodes/<node>/qemu/<id>/vncwebsocket?port=<port>&vncticket=<ticket>`:
    /// it holds the ticket.
    pub websocket: SecretValue,
    /// The VNC port `vncproxy` opened on the node (in the URL already).
    pub port: u16,
    /// The ticket: the VNC password, and in the URL.
    pub ticket: SecretValue,
    /// The header that signs the websocket in: its name (`Authorization`, `Cookie`) and value.
    pub authorization: (String, SecretValue),
    /// The server's certificate isn't verified (a self-signed one: the provider's own `insecure`
    /// setting).
    pub insecure: bool,
}

/// A machine's graphical console the runner reached: where it answers, and how to sign in.
#[derive(Debug)]
#[non_exhaustive]
pub struct Console {
    /// The machine.
    pub machine: String,
    /// The protocol it speaks: VNC or RDP.
    pub protocol: ConsoleProtocol,
    /// Where it answers.
    pub endpoint: Endpoint,
    /// The account to sign in as, when it signs in to the machine's own OS (a Windows VM's
    /// Remote Desktop: its administrator).
    pub user: Option<String>,
    /// Its password (Proxmox's ticket, a Windows VM's administrator password, from the module's
    /// sensitive output). Never print or log it.
    pub password: Option<SecretValue>,
    /// What to know of it (the extension pack VirtualBox's takes, the machine a tunnel goes
    /// through).
    pub note: Option<String>,
    /// The SSH tunnel it is reached through, when it is ([`Endpoint::Loopback`] is the tunnel's
    /// end): open for as long as this console is kept, ended when it is dropped. A viewer keeps
    /// the console while it shows the screen, and can wait on [`Tunnel::closed`] beside its own
    /// end.
    pub tunnel: Option<Tunnel>,
    /// Where the tunnel goes, as the driver read it, until the core opens it
    /// ([`crate::Running::graphical_console`]): the endpoint is no address until then.
    pub(crate) through: Option<tunnel::To>,
}

impl Console {
    /// `machine`'s console speaking `protocol` at `endpoint`, reached through no tunnel: what an
    /// engine other than the runner's gives (the runner's service answers through one).
    #[must_use]
    pub fn new(machine: &str, protocol: ConsoleProtocol, endpoint: Endpoint) -> Console {
        Console {
            machine: machine.to_owned(),
            protocol,
            endpoint,
            user: None,
            password: None,
            note: None,
            tunnel: None,
            through: None,
        }
    }

    /// `machine`'s console speaking `protocol`, reached through a tunnel to `to` the core opens:
    /// its endpoint is the tunnel's end once it is.
    pub(crate) fn tunnelled(machine: &str, protocol: ConsoleProtocol, to: tunnel::To) -> Console {
        let unopened = Endpoint::Loopback(SocketAddr::from(([127, 0, 0, 1], 0)));
        let mut console = Console::new(machine, protocol, unopened);
        console.through = Some(to);
        console
    }

    /// The console, saying `note`.
    pub(crate) fn with_note(mut self, note: Option<&str>) -> Console {
        self.note = note.map(str::to_owned);
        self
    }
}

/// [`Error::NoConsole`] for `machine`, saying `why`.
pub(crate) fn no_console(machine: &str, why: impl Into<String>) -> Error {
    Error::NoConsole {
        machine: machine.to_owned(),
        why: why.into(),
    }
}

/// `at`, when it is on this computer's loopback; refused otherwise, whatever the hypervisor says:
/// the runner never hands out a console on the network.
pub(crate) fn on_loopback(machine: &str, at: SocketAddr) -> crate::Result<SocketAddr> {
    if at.ip().is_loopback() {
        Ok(at)
    } else {
        Err(no_console(
            machine,
            format!("its console listens on {at}, not on this computer's loopback: the runner refuses it"),
        ))
    }
}

/// Why `found` (an attempt to reach a console) gives none, as a status says it: the error's own
/// words for a console the hypervisor doesn't give, any other error's text.
pub(crate) fn why_not<T>(found: crate::Result<T>) -> Option<String> {
    match found {
        Ok(_) => None,
        Err(Error::NoConsole { why, .. }) => Some(why),
        Err(e) => Some(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_console_off_the_loopback_is_refused() {
        assert!(on_loopback("web", "127.0.0.1:5900".parse().unwrap()).is_ok());
        assert!(on_loopback("web", "[::1]:5900".parse().unwrap()).is_ok());
        for lan in ["0.0.0.0:5900", "192.168.1.20:5900", "[::]:5900"] {
            let err = on_loopback("web", lan.parse().unwrap()).unwrap_err();
            assert_eq!(err.code(), crate::ErrorCode::NoConsole, "{lan}");
            assert!(err.to_string().contains("not on this computer's loopback"), "{err}");
        }
    }

    #[test]
    fn an_offer_says_its_protocol_and_why_it_cant_be_shown() {
        let offer = ConsoleOffer::of(ConsoleProtocol::Rdp, Some("needs the extension pack")).unavailable(Some("off".into()));
        let json = serde_json::to_value(&offer).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "protocol": "rdp", "note": "needs the extension pack", "unavailable": "off" })
        );
        assert_eq!(serde_json::to_value(ConsoleOffer::none("x")).unwrap()["protocol"], "none");
    }
}
