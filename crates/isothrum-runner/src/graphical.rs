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
pub(crate) mod native;
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

pub use native::{NativeApp, NativeConsole, NativeNeed, NativeOpen};
pub use proxmox::ProxmoxVnc;
pub(crate) use proxmox::vm_states as proxmox_vm_states;

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
    /// Why it can't be shown now, as the status found it on a running machine: its hypervisor
    /// gives it no display (a VM started without one), or it doesn't answer where the hypervisor
    /// says. None when it answers, or wasn't looked at: the machine doesn't run, it has none, or
    /// it is reached through an SSH tunnel or Proxmox's API, which a status doesn't open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

impl ConsoleOffer {
    /// What `machine`'s manifest offers under `provider`: `None` when it says nothing (a
    /// container, a manifest written before consoles were).
    #[must_use]
    pub(crate) fn of(machine: &Machine, provider: Option<Provider>) -> Option<ConsoleOffer> {
        machine.console(provider).map(|c| ConsoleOffer {
            protocol: c.protocol,
            note: c.note.clone(),
            unavailable: None,
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

/// How long a console on the loopback gets to accept a connection before it is said not to
/// answer.
const ANSWER_LIMIT: std::time::Duration = std::time::Duration::from_secs(2);

/// `at`, once a connection to it is accepted within [`ANSWER_LIMIT`]; refused otherwise, saying
/// where it didn't answer.
async fn answering(cx: &Cx<'_>, machine: &Machine, at: SocketAddr) -> Result<Endpoint> {
    let ports = &cx.run.runner.config.ports;
    if ports.reachable(&at.ip().to_string(), at.port(), ANSWER_LIMIT).await {
        Ok(Endpoint::Loopback(at))
    } else {
        Err(none(machine, format!("its screen didn't answer at {at}")))
    }
}

/// Where the hypervisor of this computer says `machine`'s console `console` answers, with the
/// password the runner chose for it, when the hypervisor keeps it (libvirt, QEMU, VirtualBox,
/// the runner's own); `None` for one reached through Proxmox's API or an SSH tunnel.
async fn local(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console) -> Option<Result<(SocketAddr, Option<SecretValue>)>> {
    let at = |endpoint: Result<Endpoint>| match endpoint {
        Ok(Endpoint::Loopback(at)) => Ok((at, None)),
        Ok(Endpoint::ProxmoxVnc(_)) => Err(none(machine, "its hypervisor gives no port of this computer")),
        Err(e) => Err(e),
    };
    Some(match console.source.as_ref()? {
        ConsoleSource::Libvirt {} => at(vagrant::libvirt(cx, machine).await),
        ConsoleSource::QemuMonitor {} => at(vagrant::qemu(cx, machine).await),
        ConsoleSource::Vrde {} => at(vagrant::vrde(cx, machine).await),
        ConsoleSource::Runner {} => chosen::of(cx, machine).map(|(port, password)| (SocketAddr::new(LOOPBACK, port), Some(password))),
        ConsoleSource::ProxmoxApi { .. } | ConsoleSource::SshTunnel { .. } => return None,
    })
}

/// Why `machine`'s console `console` (one [`offered`] gave) can't be shown now, the machine
/// running: its hypervisor gives it no display, or it doesn't answer there. None when it answers,
/// or is reached through Proxmox's API or an SSH tunnel, which this doesn't open.
pub(crate) async fn unavailable(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console) -> Option<String> {
    let found = match local(cx, machine, console).await? {
        Ok((at, _)) => answering(cx, machine, at).await.map(drop),
        Err(e) => Err(e),
    };
    match found {
        Ok(()) => None,
        Err(Error::NoConsole { why, .. }) => Some(why),
        Err(e) => Some(e.to_string()),
    }
}

/// Reaches `machine`'s console `console` (one [`offered`] gave), the machine running and the
/// run's outputs bound in `cx`: one on the loopback only once it answers there.
pub(crate) async fn reach(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console, outputs: &[crate::OutputValue]) -> Result<Console> {
    let at = |field: &str| format!("machines.{}.consoles.source.{field}", machine.name);
    if let Some(found) = local(cx, machine, console).await {
        let (address, password) = found?;
        let mut c = Console::new(machine, console, answering(cx, machine, address).await?);
        c.password = password;
        return with_account(cx, machine, console, outputs, c);
    }
    let reached = match console.source.as_ref() {
        None => return Err(none(machine, "it has none")),
        Some(ConsoleSource::Libvirt {} | ConsoleSource::QemuMonitor {} | ConsoleSource::Vrde {} | ConsoleSource::Runner {}) => {
            return Err(none(machine, "its hypervisor didn't say where it is"));
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
    with_account(cx, machine, console, outputs, reached)
}

/// `reached` with the account `console` signs in as, when it names one.
fn with_account(cx: &Cx<'_>, machine: &Machine, console: &isoloom_manifest::Console, outputs: &[crate::OutputValue], mut reached: Console) -> Result<Console> {
    let at = |field: &str| format!("machines.{}.consoles.source.{field}", machine.name);
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
            { "providers": ["parallels"], "protocol": "none", "note": "no console set up" },
        ]));
        assert_eq!(offered(&m, Some(Provider::Libvirt)).unwrap().protocol, ConsoleProtocol::Vnc);
        let parallels = offered(&m, Some(Provider::Parallels)).unwrap_err();
        assert_eq!(
            (parallels.code(), parallels.to_string()),
            (
                crate::ErrorCode::NoConsole,
                "machine `web` has no graphical console: no console set up".to_owned()
            )
        );
        let other = offered(&m, Some(Provider::Hyperv)).unwrap_err();
        assert!(other.to_string().ends_with("the run's manifest lists no graphical console for it"), "{other}");
        assert_eq!(
            ConsoleOffer::of(&m, Some(Provider::Parallels)),
            Some(ConsoleOffer {
                protocol: ConsoleProtocol::None,
                note: Some("no console set up".into()),
                unavailable: None,
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
