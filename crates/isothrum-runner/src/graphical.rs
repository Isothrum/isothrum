//! A machine's graphical console (its screen, keyboard and mouse), found where its manifest says
//! (`machines[].consoles`): [`Console`], what [`crate::Running::graphical_console`] gives, and
//! [`ConsoleOffer`], what a status says of each machine's.
//!
//! The runner guarantees where a console answers: a TCP port of this computer's loopback
//! ([`Endpoint::Loopback`]), refusing one the hypervisor says listens anywhere else, opening an
//! SSH tunnel to it when the manifest says so ([`tunnel`], held by the [`Console`]), or, for
//! Proxmox VE, its API's own VNC websocket with a ticket for this session
//! ([`Endpoint::ProxmoxVnc`]).
//!
//! - **libvirt**: `virsh domdisplay <domain>` ([`vagrant::libvirt`]).
//! - **QEMU** (vagrant-qemu): `info vnc` on the VM's monitor socket ([`vagrant::qemu`]).
//! - **VirtualBox**: `VBoxManage showvminfo --machinereadable` ([`vagrant::vrde`]).
//! - **The runner's own** (VMware): the port and password it chose for the run, kept in the
//!   run's `consoles.json` ([`chosen`]).
//! - **Proxmox**: `vncproxy` over its API with `curl`, its credentials from a config file the
//!   runner writes owner-only and removes ([`proxmox`]).
//! - **An SSH tunnel**: `ssh -N -L 127.0.0.1:<port>:<host>:<port>` ([`tunnel`]).

pub(crate) mod chosen;
mod proxmox;
mod tunnel;
mod vagrant;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use isoloom_manifest::{ConsoleProtocol, ConsoleSource, Machine, Provider};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::secrets::SecretValue;

pub use proxmox::ProxmoxVnc;

/// What a status says of a machine's graphical console under the run's provider: its protocol,
/// or `none` and why ([`isoloom_manifest::Console`]'s, as the manifest says it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ConsoleOffer {
    /// `vnc`, `rdp`, or `none`.
    pub protocol: ConsoleProtocol,
    /// Why there is none, or what it needs (an extension pack).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ConsoleOffer {
    /// What `machine`'s manifest offers under `provider`: `None` when it says nothing (a
    /// container, a manifest written before consoles were).
    #[must_use]
    pub(crate) fn of(machine: &Machine, provider: Option<Provider>) -> Option<ConsoleOffer> {
        machine.console(provider).map(|c| ConsoleOffer {
            protocol: c.protocol,
            note: c.note.clone(),
        })
    }
}

/// Where a graphical console answers.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Endpoint {
    /// A TCP port of this computer's loopback speaking the console's protocol (VNC, RDP): the
    /// hypervisor's own, or a tunnel's the [`Console`] holds.
    Loopback(SocketAddr),
    /// Proxmox VE's VNC websocket, through its API.
    ProxmoxVnc(ProxmoxVnc),
}

/// A machine's graphical console the runner reached: where it answers, and how to sign in. A
/// tunnel it opened lives as long as the console: drop it once the viewer is done.
#[derive(Debug)]
#[non_exhaustive]
pub struct Console {
    /// The machine.
    pub machine: String,
    /// The protocol it speaks: VNC or RDP.
    pub protocol: ConsoleProtocol,
    /// Where it answers.
    pub endpoint: Endpoint,
    /// The account to sign in as, when it signs in to the machine's own OS (RDP to Windows).
    pub user: Option<String>,
    /// Its password: the VNC password, or the account's. Never print or log it.
    pub password: Option<SecretValue>,
    /// What to know of it, as the manifest says (the extension pack VirtualBox's takes).
    pub note: Option<String>,
    /// The SSH tunnel it is reached through, while the console lives.
    tunnel: Option<tunnel::Tunnel>,
}

impl Console {
    /// A console of `machine` at `endpoint`, as `console` says.
    fn new(machine: &Machine, console: &isoloom_manifest::Console, endpoint: Endpoint) -> Console {
        Console {
            machine: machine.name.to_string(),
            protocol: console.protocol,
            endpoint,
            user: None,
            password: None,
            note: console.note.clone(),
            tunnel: None,
        }
    }

    /// Whether it is reached through an SSH tunnel the runner holds open.
    #[must_use]
    pub fn tunnelled(&self) -> bool {
        self.tunnel.is_some()
    }
}

/// The loopback address the runner's own listeners and tunnels take.
pub(crate) const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// [`Error::NoConsole`] for `machine`, saying `why`.
pub(crate) fn none(machine: &Machine, why: impl Into<String>) -> Error {
    Error::NoConsole {
        machine: machine.name.to_string(),
        why: why.into(),
    }
}

/// `machine`'s console under `provider`, as the manifest gives it; refused when it lists none or
/// says it has none (why, its note).
pub(crate) fn offered(machine: &Machine, provider: Option<Provider>) -> Result<&isoloom_manifest::Console> {
    let Some(console) = machine.console(provider) else {
        return Err(none(machine, "the run's manifest lists no graphical console for it"));
    };
    if console.protocol == ConsoleProtocol::None || console.source.is_none() {
        return Err(none(machine, console.note.clone().unwrap_or_else(|| "it has none".to_owned())));
    }
    Ok(console)
}

/// `at`, when it is on this computer's loopback; refused otherwise, whatever the hypervisor says:
/// the runner never hands out a console on the network.
pub(crate) fn on_loopback(machine: &Machine, at: SocketAddr) -> Result<Endpoint> {
    if at.ip().is_loopback() {
        Ok(Endpoint::Loopback(at))
    } else {
        Err(none(
            machine,
            format!("its console listens on {at}, not on this computer's loopback: the runner refuses it"),
        ))
    }
}

/// Reaches `machine`'s console `console` (one [`offered`] gave), the machine running and the
/// run's outputs bound in `cx`.
pub(crate) async fn reach(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console, outputs: &[crate::OutputValue]) -> Result<Console> {
    let at = |field: &str| format!("machines.{}.consoles.source.{field}", machine.name);
    let mut reached = match console.source.as_ref() {
        None => return Err(none(machine, "it has none")),
        Some(ConsoleSource::Libvirt {}) => Console::new(machine, console, vagrant::libvirt(cx, machine).await?),
        Some(ConsoleSource::QemuMonitor {}) => Console::new(machine, console, vagrant::qemu(cx, machine).await?),
        Some(ConsoleSource::Vrde {}) => Console::new(machine, console, vagrant::vrde(cx, machine).await?),
        Some(ConsoleSource::Runner {}) => {
            let (port, password) = chosen::of(cx, machine)?;
            let mut c = Console::new(machine, console, Endpoint::Loopback(SocketAddr::new(LOOPBACK, port)));
            c.password = Some(password);
            c
        }
        Some(ConsoleSource::ProxmoxApi { .. }) => {
            let vnc = proxmox::vnc(cx, machine, console).await?;
            let mut c = Console::new(machine, console, Endpoint::ProxmoxVnc(vnc.clone()));
            c.password = Some(vnc.ticket.clone());
            c
        }
        Some(ConsoleSource::SshTunnel { via, user, host, port }) => {
            let via = cx.vars.render(via, &at("via"))?;
            let user = cx.vars.render(user, &at("user"))?;
            let host = cx.vars.render(host, &at("host"))?;
            let open = tunnel::open(cx, machine, &tunnel::To { via, user, host, port: *port }).await?;
            let mut c = Console::new(machine, console, Endpoint::Loopback(open.local));
            c.tunnel = Some(open);
            c
        }
    };
    if let Some(account) = &console.account {
        reached.user = Some(cx.vars.render(&account.user, &at("account.user"))?);
        reached.password = Some(SecretValue::new(revealed(cx, &account.password, outputs, &at("account.password"))?));
    }
    Ok(reached)
}

/// Where a tunnel goes, for the run's tests.
#[cfg(test)]
pub(crate) fn tunnel_to_for_tests(via: &str, user: &str, host: &str, port: u16) -> tunnel::To {
    tunnel::To {
        via: via.into(),
        user: user.into(),
        host: host.into(),
        port,
    }
}

/// A tunnel's `ssh`, for the run's tests.
#[cfg(test)]
pub(crate) fn tunnel_spec_for_tests(cx: &Cx<'_>, to: &tunnel::To, local: u16) -> Result<crate::process::ProcessSpec> {
    tunnel::spec(cx, to, local)
}

/// `template` filled, a sensitive output it is alone made of revealed (one a template is never
/// otherwise given: a password the viewer signs in with).
fn revealed(cx: &Cx<'_>, template: &isoloom_manifest::Template, outputs: &[crate::OutputValue], at: &str) -> Result<String> {
    if let [isoloom_manifest::Part::Placeholder(isoloom_manifest::Placeholder::Output(name))] = template.parts()
        && let Some(value) = outputs.iter().find(|o| o.name == name.as_str()).and_then(crate::OutputValue::revealed)
    {
        return Ok(value.to_owned());
    }
    cx.vars.render(template, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(consoles: &serde_json::Value) -> Machine {
        serde_json::from_value(serde_json::json!({ "name": "web", "role": "member", "addresses": {}, "consoles": consoles })).unwrap()
    }

    #[test]
    fn a_machine_offers_its_providers_console_or_says_why_not() {
        let m = machine(&serde_json::json!([
            { "providers": ["libvirt"], "protocol": "vnc", "source": { "from": "libvirt" } },
            { "providers": ["utm"], "protocol": "none", "note": "SPICE only" },
        ]));
        assert_eq!(offered(&m, Some(Provider::Libvirt)).unwrap().protocol, ConsoleProtocol::Vnc);
        let utm = offered(&m, Some(Provider::Utm)).unwrap_err();
        assert_eq!(
            (utm.code(), utm.to_string()),
            (crate::ErrorCode::NoConsole, "machine `web` has no graphical console: SPICE only".to_owned())
        );
        let other = offered(&m, Some(Provider::Hyperv)).unwrap_err();
        assert!(other.to_string().ends_with("the run's manifest lists no graphical console for it"), "{other}");
        assert_eq!(
            ConsoleOffer::of(&m, Some(Provider::Utm)),
            Some(ConsoleOffer {
                protocol: ConsoleProtocol::None,
                note: Some("SPICE only".into())
            })
        );
        assert_eq!(ConsoleOffer::of(&machine(&serde_json::json!([])), None), None, "a container says nothing");
    }

    #[test]
    fn a_console_off_the_loopback_is_refused() {
        let m = machine(&serde_json::json!([]));
        assert!(matches!(on_loopback(&m, "127.0.0.1:5900".parse().unwrap()), Ok(Endpoint::Loopback(_))));
        assert!(matches!(on_loopback(&m, "[::1]:5900".parse().unwrap()), Ok(Endpoint::Loopback(_))));
        for lan in ["0.0.0.0:5900", "192.168.1.20:5900", "[::]:5900"] {
            let err = on_loopback(&m, lan.parse().unwrap()).unwrap_err();
            assert_eq!(err.code(), crate::ErrorCode::NoConsole, "{lan}");
            assert!(err.to_string().contains("not on this computer's loopback"), "{err}");
        }
    }
}
