//! Picking the host ports a fresh run pins: a free
//! loopback port for each ephemeral publish, every socket held until all are picked so the OS
//! doesn't hand one out twice, and each pick reserved in the runner until the run's start ends, so
//! another run of this runner starting meanwhile can't take it either. With the
//! `ISOLOOM_PUBLISH_FIXED` param set, a port takes its manifest's `preferred` number instead, once
//! per number and protocol.

use super::config::{self, ConfigPort};
use crate::Result;
use crate::driver::port_pick::PortPick;
use crate::error::Error;
use crate::executor::Cx;

/// The pinned-ports override for `config` (`docker compose config --format json`) that `step`
/// read, or `None` when nothing is ephemeral. A config that can't be read, or a port with no free
/// host port left to pin it to, fails the step: the run would otherwise start on ephemeral ports
/// and a resume would get new addresses.
pub(super) fn pick(cx: &Cx<'_>, step: &str, config: &str) -> Result<Option<String>> {
    let ports = config::ports(config).ok_or_else(|| config::unreadable(step, "the host ports to pin are"))?;
    let fixed = cx.vars.params().contains_key(crate::driver::PUBLISH_FIXED);
    let mut pick = PortPick::new(cx);
    config::pinned_ports(&ports, |p| {
        if fixed && let Some(preferred) = preferred(cx, p).and_then(|n| pick.fixed(n, p.protocol)) {
            return Ok(preferred);
        }
        pick.free(p.protocol).ok_or_else(|| Error::ToolFailed {
            step: step.to_owned(),
            reason: format!(
                "no free host port over {} is left to pin service `{}`'s container port {} to",
                crate::encoding::wire_name(&p.protocol),
                p.service,
                p.target
            ),
        })
    })
}

/// The manifest's `preferred` number for the machine port `p` publishes.
fn preferred(cx: &Cx<'_>, p: &ConfigPort) -> Option<u16> {
    let machine = cx.run.manifest().machines.iter().find(|m| crate::driver::native_name(m) == p.service)?;
    let port = machine.ports.iter().find(|q| q.port.get() == p.target && q.kind.transport() == p.protocol)?;
    port.publish.as_ref()?.preferred.map(std::num::NonZeroU16::get)
}
