//! Picking the host ports a fresh run pins: a free
//! loopback port for each ephemeral publish, every socket held until all are picked so the OS
//! doesn't hand one out twice, and each pick reserved in the runner until the run's start ends, so
//! another run of this runner starting meanwhile can't take it either. With the
//! `ISOLOOM_PUBLISH_FIXED` param set, a port takes its manifest's `preferred` number instead, once
//! per number and protocol.

use std::collections::BTreeSet;

use isoloom_manifest::{Native, Protocol};

use super::config::{self, ConfigPort};
use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::net::HeldPort;

/// The pinned-ports override for `config` (`docker compose config --format json`) that `step`
/// read, or `None` when nothing is ephemeral. A config that can't be read, or a port with no free
/// host port left to pin it to, fails the step: the run would otherwise start on ephemeral ports
/// and a resume would get new addresses.
pub(super) fn pick(cx: &Cx<'_>, step: &str, config: &str) -> Result<Option<String>> {
    let ports = config::ports(config).ok_or_else(|| config::unreadable(step, "the host ports to pin are"))?;
    let fixed = cx.vars.params().contains_key(crate::driver::PUBLISH_FIXED);
    let mut held: Vec<HeldPort> = Vec::new();
    let mut taken: BTreeSet<(u16, Protocol)> = BTreeSet::new();
    let pins = config::pinned_ports(&ports, |p| {
        let mine = |n: &u16| cx.run.runner.reserve(cx.run.name(), *n, p.protocol);
        if fixed && let Some(preferred) = preferred(cx, p).filter(|n| !taken.contains(&(*n, p.protocol)) && mine(n)) {
            taken.insert((preferred, p.protocol));
            return Ok(preferred);
        }
        let port = free_port(cx, p.protocol).ok_or_else(|| Error::ToolFailed {
            step: step.to_owned(),
            reason: format!(
                "no free host port over {} is left to pin service `{}`'s container port {} to",
                crate::encoding::wire_name(&p.protocol),
                p.service,
                p.target
            ),
        })?;
        taken.insert((port.port, p.protocol));
        let number = port.port;
        held.push(port);
        Ok(number)
    });
    drop(held);
    pins
}

/// A free port over `protocol` that no other run of this runner holds, reserved for this run.
fn free_port(cx: &Cx<'_>, protocol: Protocol) -> Option<HeldPort> {
    cx.run.runner.pick_reserved(cx.run.name(), protocol)
}

/// The manifest's `preferred` number for the machine port `p` publishes.
fn preferred(cx: &Cx<'_>, p: &ConfigPort) -> Option<u16> {
    let machine = cx.run.manifest().machines.iter().find(|m| match &m.native {
        Some(Native::Compose { service }) => service.as_str() == p.service,
        _ => m.name.as_str() == p.service,
    })?;
    let port = machine.ports.iter().find(|q| q.port.get() == p.target && q.kind.transport() == p.protocol)?;
    port.publish.as_ref()?.preferred.map(std::num::NonZeroU16::get)
}
