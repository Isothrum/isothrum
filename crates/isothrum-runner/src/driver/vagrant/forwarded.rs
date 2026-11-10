//! `vagrant.check_forwarded_ports`: a VM forwards each published port from this computer, at its
//! own `publish` number when it is `fixed`, or at the one the runner picks (and pins for the VM's
//! life) when it is `ephemeral` ([`pick`]; at its `publish` number for a run that recorded no pick:
//! [`host_port`]). Two runs forwarding one fixed number collide, and Vagrant says so only once the
//! VM was imported, so unless the run's VMs exist (their forwards are this run's own) each host
//! port it will forward must be free over its protocol, and not reserved by another run of this
//! runner starting meanwhile.

use std::collections::BTreeMap;

use isoloom_manifest::{Machine, Manifest, Port, Protocol, Provider, PublishMode, Step};

use super::status;
use crate::Result;
use crate::driver::port_pick::{self, PortPick};
use crate::executor::Cx;
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
/// `tool_failed` ([`PICK_STEP`]) when no free host port is left for one: as Compose's pick does.
pub(in crate::driver) fn pick(cx: &Cx<'_>) -> Result<BTreeMap<String, u16>> {
    let mut chosen = BTreeMap::new();
    if cx.scope.runs_on(Provider::VmwareEsxi) {
        return Ok(chosen);
    }
    let mut pick = PortPick::new(cx);
    for m in &cx.run.manifest().machines {
        for p in m.ports.iter().filter(|p| ephemeral(p)) {
            let what = format!("machine `{}`'s port `{}`", m.name, p.name);
            chosen.insert(key(m, p), pick.pin(PICK_STEP, &what, port_pick::preferred(p), p.kind.transport())?);
        }
    }
    // The helper VM's own SSH forward on QEMU: vagrant-qemu forwards guest 22 to one fixed host
    // port (50022) with weak auto-correct, so two local VMs — or a half-created one left by a
    // failed start — collide on it. Give it a free host port of its own, the same way a published
    // port gets one; only QEMU needs it (every other provider manages its own SSH access).
    if cx.scope.runs_on(Provider::Qemu)
        && let Some(ssh) = ssh_key(cx.run.manifest())
    {
        let port = pick.pin(PICK_STEP, "the helper VM's SSH forward", None, Protocol::Tcp)?;
        chosen.insert(ssh, port);
    }
    Ok(chosen)
}

/// The host port `m`'s published port `p` is forwarded from on this computer, as the
/// Vagrantfile reads it (`ISOLOOM_HOSTPORTS`, else the port's publish number): the one the runner
/// pinned for it among `forwards`, else its `preferred` number, else its own, whatever its mode.
/// `None` when `p` isn't published.
pub(super) fn host_port(m: &Machine, p: &Port, forwards: &BTreeMap<String, u16>) -> Option<u16> {
    let pinned = || forwards.get(&key(m, p)).copied();
    p.publish
        .as_ref()
        .map(|_| pinned().or_else(|| port_pick::preferred(p)).unwrap_or_else(|| p.port.get()))
}

/// The host ports this run forwards from this computer: each published port's ([`host_port`]).
fn host_ports(cx: &Cx<'_>) -> Vec<(u16, Protocol)> {
    let forwards = cx.vars.forwards();
    let mut ports = Vec::new();
    for m in &cx.run.manifest().machines {
        for p in &m.ports {
            if let Some(port) = host_port(m, p, forwards) {
                ports.push((port, p.kind.transport()));
            }
        }
    }
    // The helper VM's SSH forward (chosen in `pick`, present only on a QEMU docker-vm run).
    if let Some(ssh) = ssh_key(cx.run.manifest())
        && let Some(port) = forwards.get(&ssh)
    {
        ports.push((*port, Protocol::Tcp));
    }
    ports
}

/// `vagrant.check_forwarded_ports` (step `step`): unless the run's VMs exist (their forwards are
/// its own), every host port the run will forward ([`host_ports`]) must be free, and not reserved
/// by another run of this runner; the first that isn't fails the step (`port_in_use`).
pub(super) async fn check(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let read = status::parse(&super::lock::read_status(cx).await?);
    if read.states.values().any(|s| status::machine_state(s) != MachineState::Absent) {
        tracing::debug!(step = %step.id, "the run's VMs exist: their forwards are its own");
        return Ok(());
    }
    cx.run.runner.ensure_ports_free(cx.run.name(), host_ports(cx))
}
