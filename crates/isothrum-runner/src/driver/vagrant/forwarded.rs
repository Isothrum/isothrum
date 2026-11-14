//! Where a VM's published ports are reached ([`reached_at`], the one way from a VM's port to a
//! host port), and `vagrant.check_forwarded_ports`: a VM forwards each published port from this
//! computer, at its own `publish` number when it is `fixed`, or at the one the runner picks (and
//! pins for the VM's life) when it is `ephemeral` ([`pin`]; at its `publish` number for a run
//! that recorded no pick). Two runs forwarding one fixed number collide, and Vagrant says so only once the
//! VM was imported, so unless the run's VMs exist (their forwards are this run's own) each host
//! port it will forward must be free over its protocol, and not reserved by another run of this
//! runner starting meanwhile.

use std::collections::BTreeMap;

use isoloom_manifest::{Machine, Manifest, Port, Protocol, Provider, PublishMode, Step};

use super::status;
use crate::Result;
use crate::driver::port_pick::PortPick;
use crate::executor::Cx;
use crate::port::{GuestPort, HostPort, PreferredPort};
use crate::status::MachineState;

/// The key a chosen host port is held under: `"<machine>/<port>"`.
pub(super) fn key(m: &Machine, p: &Port) -> String {
    format!("{}/{}", m.name, p.name)
}

/// The key the helper VM's SSH forward host port is held under (`"<guest machine>/ssh"`), for a
/// run that runs Compose inside a VM (Isoloom's docker-vm). The Vagrantfile's QEMU provider block
/// reads it as that VM's `ssh_port`; no machine of the run has a port named `ssh`, so it can't
/// clash with a published port's key.
fn ssh_key(manifest: &Manifest) -> Option<String> {
    manifest.guest.as_ref().map(|g| format!("{}/ssh", g.machine.as_str()))
}

/// Whether `p` is a published port the runner picks a free host port for.
fn ephemeral(p: &Port) -> bool {
    p.publish.as_ref().is_some_and(|x| x.mode == PublishMode::Ephemeral)
}

/// The pseudo-step a fresh Vagrant start's host-port pick fails as: it comes before any step.
const PICK_STEP: &str = "host-ports";

/// The host ports a fresh local Vagrant start pins, by `"<machine>/<port>"`, by
/// [`PortPick::pin`]'s rule: a free loopback port for each ephemeral published port (every socket
/// held until all are picked so the OS doesn't hand one out twice), or the port's `preferred`
/// number when `ISOLOOM_PUBLISH_FIXED` asked for it and no other run reserved it. Empty for an
/// ESXi run (which forwards nothing: a VM's ports answer at its own address) or when no published
/// port is ephemeral.
///
/// # Errors
///
/// `tool_failed` ([`PICK_STEP`]) when no free host port is found for one, `io` when the loopback
/// can't be bound to look for one: as Compose's pick does ([`PortPick::pin`]).
pub(in crate::driver) fn pin(cx: &Cx<'_>) -> Result<BTreeMap<String, HostPort>> {
    let mut chosen = BTreeMap::new();
    if cx.scope.runs_on(Provider::VmwareEsxi) {
        return Ok(chosen);
    }
    let mut pick = PortPick::new(cx);
    for m in &cx.manifest().machines {
        for p in m.ports.iter().filter(|p| ephemeral(p)) {
            let what = format!("machine `{}`'s port `{}`", m.name, p.name);
            chosen.insert(key(m, p), pick.pin(PICK_STEP, &what, PreferredPort::of(p), p.kind.transport())?);
        }
    }
    // The helper VM's own SSH forward on QEMU: vagrant-qemu forwards guest 22 to one fixed host
    // port (50022) with weak auto-correct, so two local VMs — or a half-created one left by a
    // failed start — collide on it. Give it a free host port of its own, the same way a published
    // port gets one; only QEMU needs it (every other provider manages its own SSH access).
    if cx.scope.runs_on(Provider::Qemu)
        && let Some(ssh) = ssh_key(cx.manifest())
    {
        let port = pick.pin(PICK_STEP, "the helper VM's SSH forward", None, Protocol::Tcp)?;
        chosen.insert(ssh, port);
    }
    Ok(chosen)
}

/// Where a run's VMs' published ports are reached: at the VM's own address (ESXi, which forwards
/// nothing), or forwarded from this computer.
#[derive(Clone, Copy)]
pub(in crate::driver) enum Forwarding<'a> {
    /// The VM answers at its own address, on the port itself.
    AtVm,
    /// Forwarded from this computer, at the runner's pick among these (by `"<machine>/<port>"`)
    /// first.
    Local(&'a BTreeMap<String, HostPort>),
}

impl Forwarding<'_> {
    /// How `cx`'s run's VMs are reached: at their own address on ESXi, else forwarded from this
    /// computer at the ports the run pinned.
    pub(in crate::driver) fn of<'c>(cx: &'c Cx<'_>) -> Forwarding<'c> {
        if cx.scope.runs_on(Provider::VmwareEsxi) {
            Forwarding::AtVm
        } else {
            Forwarding::Local(cx.vars.vm_pins())
        }
    }
}

/// The host port `m`'s published port `p` is reached at, the one way from a VM's port to a host
/// port: at the VM's own address, the port itself; forwarded from this computer, as the
/// Vagrantfile reads it (`ISOLOOM_HOSTPORTS`, else the port's publish number): the one the runner
/// pinned for it, else its `preferred` number, else its own, whatever its mode. `None` when `p`
/// isn't published (nothing forwards it).
pub(in crate::driver) fn reached_at(forwarding: Forwarding<'_>, m: &Machine, p: &Port) -> Option<HostPort> {
    p.publish.as_ref()?;
    let guest = GuestPort::of(p);
    Some(match forwarding {
        Forwarding::AtVm => guest.at_own_address(),
        Forwarding::Local(pins) => pins
            .get(&key(m, p))
            .copied()
            .or_else(|| PreferredPort::of(p).map(PreferredPort::unpicked))
            .unwrap_or_else(|| guest.forwarded_from_its_own_number()),
    })
}

/// The host ports this run forwards from this computer: each published port's ([`reached_at`]).
fn host_ports(cx: &Cx<'_>) -> Vec<(HostPort, Protocol)> {
    let forwards = cx.vars.vm_pins();
    let mut ports = Vec::new();
    for m in &cx.manifest().machines {
        for p in &m.ports {
            if let Some(port) = reached_at(Forwarding::Local(forwards), m, p) {
                ports.push((port, p.kind.transport()));
            }
        }
    }
    // The helper VM's SSH forward (chosen in `pick`, present only on a QEMU docker-vm run).
    if let Some(ssh) = ssh_key(cx.manifest())
        && let Some(port) = forwards.get(&ssh)
    {
        ports.push((*port, Protocol::Tcp));
    }
    ports
}

/// `vagrant.check_forwarded_ports` (step `step`): unless the run's VMs exist (their forwards are
/// its own), every host port the run will forward ([`host_ports`]) must be free, and not reserved
/// by another run of this home; the first that isn't fails the step (`port_in_use`).
pub(super) async fn check(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let read = status::parse(&super::lock::read_status_output(cx).await?);
    if read.states.values().any(|s| status::machine_state(s) != MachineState::Absent) {
        tracing::debug!(step = %step.id, "the run's VMs exist: their forwards are its own");
        return Ok(());
    }
    cx.run.ensure_ports_free(host_ports(cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    use isoloom_manifest::Publish;

    /// The web machine with its one port, `portal` (80), published as `publish`.
    fn machine(publish: Option<Publish>) -> Machine {
        let mut web = crate::testing::docker_manifest()
            .machines
            .into_iter()
            .find(|m| m.name.as_str() == "web")
            .unwrap();
        web.ports.truncate(1);
        assert_eq!(web.ports[0].port.get(), 80);
        web.ports[0].publish = publish;
        web
    }

    fn publish(mode: PublishMode, preferred: Option<u16>) -> Publish {
        Publish {
            mode,
            preferred: preferred.and_then(std::num::NonZeroU16::new),
            address: None,
        }
    }

    /// Where a VM's port is reached, by every rule: the VM's own number at its own address, else
    /// the forward's pick, else its preferred number, else its own number; nothing unpublished.
    #[test]
    fn a_vms_port_is_reached_at_its_forward_or_at_the_vm() {
        let ephemeral = machine(Some(publish(PublishMode::Ephemeral, None)));
        let preferred = machine(Some(publish(PublishMode::Fixed, Some(8080))));
        let pins = BTreeMap::from([("web/portal".to_owned(), HostPort::new(50123))]);
        let none = BTreeMap::new();
        let at = |forwarding, m: &Machine| reached_at(forwarding, m, &m.ports[0]);
        assert_eq!(at(Forwarding::AtVm, &preferred), Some(HostPort::new(80)), "ESXi: the VM's own port");
        assert_eq!(at(Forwarding::Local(&pins), &preferred), Some(HostPort::new(50123)), "the pick first");
        assert_eq!(at(Forwarding::Local(&none), &preferred), Some(HostPort::new(8080)), "then the preferred number");
        assert_eq!(at(Forwarding::Local(&none), &ephemeral), Some(HostPort::new(80)), "then its own number");
        assert_eq!(at(Forwarding::Local(&pins), &machine(None)), None, "nothing forwards an unpublished port");
        assert_eq!(at(Forwarding::AtVm, &machine(None)), None);
    }
}
