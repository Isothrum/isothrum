//! Picking the host ports a fresh run pins, by [`PortPick::pin`]'s rule: a free loopback port for
//! each ephemeral publish, or with the `ISOLOOM_PUBLISH_FIXED` param set, its manifest's
//! `preferred` number when no other run reserved it, once per number and protocol.

use super::config::{self, ConfigPort};
use crate::Result;
use crate::driver::port_pick::{self, PortPick};
use crate::executor::Cx;

/// The pinned-ports override for `config` (`docker compose config --format json`) that `step`
/// read, or `None` when nothing is ephemeral. A config that can't be read, or a port with no free
/// host port left to pin it to, fails the step: the run would otherwise start on ephemeral ports
/// and a resume would get new addresses.
pub(super) fn pick(cx: &Cx<'_>, step: &str, config: &str) -> Result<Option<String>> {
    let ports = config::ports(config).map_err(|why| config::unreadable(step, "the host ports to pin are", &why))?;
    let mut pick = PortPick::new(cx);
    config::pinned_ports(&ports, |p| {
        let what = format!("service `{}`'s container port {}", p.service, p.target);
        pick.pin(step, &what, preferred(cx, p), p.protocol)
    })
}

/// The manifest's `preferred` number for the machine port `p` publishes.
fn preferred(cx: &Cx<'_>, p: &ConfigPort) -> Option<u16> {
    let machine = cx.manifest().machines.iter().find(|m| crate::driver::native_name(m) == p.service)?;
    port_pick::preferred(super::machine_port(machine, p.target, p.protocol)?)
}
