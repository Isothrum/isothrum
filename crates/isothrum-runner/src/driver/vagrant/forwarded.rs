//! `vagrant.check_forwarded_ports`: a VM forwards each published port
//! from this computer, at its own `publish` number when it is `fixed`, or at a free one the runner
//! picks (and pins for the VM's life) when it is `ephemeral`. Two labs forwarding one fixed number
//! collide, and Vagrant says so only once the VM was imported, so unless the run's VMs exist
//! (their forwards are this run's own) each host port it will forward must be free over its
//! protocol, and not reserved by another run of this runner starting meanwhile.

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{Machine, Manifest, Port, Protocol, PublishMode, Step};

use super::status;
use crate::Result;
use crate::executor::Cx;
use crate::net::HeldPort;
use crate::status::MachineState;

/// The param asking each port for its own `publish:` number instead of a free one.
const PUBLISH_FIXED: &str = "ISOLOOM_PUBLISH_FIXED";

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

/// The host ports a fresh local Vagrant start pins, by `"<machine>/<port>"`: a free loopback port
/// for each ephemeral published port (every socket held until all are picked so the OS doesn't
/// hand one out twice), or the port's `preferred` number when `ISOLOOM_PUBLISH_FIXED` asked for it
/// and it is free. Empty for an ESXi run (which forwards nothing: a VM's ports answer at its own
/// address) or when no published port is ephemeral.
pub(super) fn pick(cx: &Cx<'_>) -> BTreeMap<String, u16> {
    let mut chosen = BTreeMap::new();
    if cx.scope.provider.as_deref() == Some(super::ESXI) {
        return chosen;
    }
    let fixed = cx.vars.params().contains_key(PUBLISH_FIXED);
    let mut held: Vec<HeldPort> = Vec::new();
    let mut taken: BTreeSet<(u16, Protocol)> = BTreeSet::new();
    for m in &cx.run.manifest().machines {
        for p in &m.ports {
            if !ephemeral(p) {
                continue;
            }
            let protocol = p.kind.transport();
            let picked = fixed.then(|| fixed_pick(cx, p, protocol, &mut taken)).flatten();
            if let Some(port) = picked.or_else(|| free_pick(cx, protocol, &mut taken, &mut held)) {
                chosen.insert(key(m, p), port);
            }
        }
    }
    // The helper VM's own SSH forward on QEMU: vagrant-qemu forwards guest 22 to one fixed host
    // port (50022) with weak auto-correct, so two local VMs — or a half-created one left by a
    // failed start — collide on it. Give it a free host port of its own, the same way a published
    // port gets one; only QEMU needs it (every other provider manages its own SSH access).
    if cx.scope.provider.as_deref() == Some(super::QEMU)
        && let Some(ssh) = ssh_key(cx.run.manifest())
        && let Some(port) = free_pick(cx, Protocol::Tcp, &mut taken, &mut held)
    {
        chosen.insert(ssh, port);
    }
    drop(held);
    chosen
}

/// `publish.preferred` when it is free to reserve for this run and not already picked.
fn fixed_pick(cx: &Cx<'_>, p: &Port, protocol: Protocol, taken: &mut BTreeSet<(u16, Protocol)>) -> Option<u16> {
    let preferred = p.publish.as_ref()?.preferred?.get();
    let free = !taken.contains(&(preferred, protocol)) && cx.run.runner.reserve(cx.run.name(), preferred, protocol);
    free.then(|| {
        taken.insert((preferred, protocol));
        preferred
    })
}

/// A free loopback port, reserved for this run and held until the whole pick is done.
fn free_pick(cx: &Cx<'_>, protocol: Protocol, taken: &mut BTreeSet<(u16, Protocol)>, held: &mut Vec<HeldPort>) -> Option<u16> {
    let port = cx.run.runner.pick_reserved(cx.run.name(), protocol)?;
    let number = port.port;
    taken.insert((number, protocol));
    held.push(port);
    Some(number)
}

/// The host ports this run forwards from this computer: each fixed port at its number, each
/// ephemeral port at the one it was pinned to (absent before a fresh start pins them).
fn host_ports(cx: &Cx<'_>) -> Vec<(u16, Protocol)> {
    let forwards = cx.vars.forwards();
    let mut ports = Vec::new();
    for m in &cx.run.manifest().machines {
        for p in &m.ports {
            let Some(publish) = p.publish.as_ref() else {
                continue;
            };
            let port = match publish.mode {
                PublishMode::Fixed => publish.preferred.map(std::num::NonZeroU16::get),
                PublishMode::Ephemeral => forwards.get(&key(m, p)).copied(),
            };
            if let Some(port) = port {
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

/// Runs the action for step `step`.
pub(super) async fn check(cx: &Cx<'_>, step: &Step) -> Result<()> {
    let read = status::parse(&crate::driver::status_report(cx).await?);
    if read.states.values().any(|s| status::machine_state(s) != MachineState::Absent) {
        tracing::debug!(step = %step.id, "the run's VMs exist: their forwards are its own");
        return Ok(());
    }
    cx.run.runner.ensure_ports_free(cx.run.name(), host_ports(cx))
}
