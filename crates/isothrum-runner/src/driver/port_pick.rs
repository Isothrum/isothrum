//! One fresh start's pick of the host ports it pins, shared by the drivers that publish on this
//! computer (Compose, Vagrant), one rule for both ([`PortPick::pin`]): a port's `preferred` number
//! when `ISOLOOM_PUBLISH_FIXED` asks for it and no other run of this runner reserved it, else a
//! free loopback port; with none left (or a loopback that can't be bound), the start fails. Each pick is reserved in the runner until
//! the start ends, so another run of this runner starting meanwhile can't take it, and each free
//! port's socket is held until the whole pick is done, so the OS doesn't hand one out twice.

use std::collections::BTreeSet;

use isoloom_manifest::{Port, Protocol};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::net::HeldPort;

/// The host port `p`'s publish asks for (`preferred`), when it is published and names one.
pub(super) fn preferred(p: &Port) -> Option<u16> {
    p.publish.as_ref()?.preferred.map(std::num::NonZeroU16::get)
}

/// The ports picked so far, with the sockets holding the free ones.
pub(super) struct PortPick<'c, 'a> {
    cx: &'c Cx<'a>,
    /// Whether the run's `ISOLOOM_PUBLISH_FIXED` param asks for each port's `preferred` number.
    fixed: bool,
    held: Vec<HeldPort>,
    taken: BTreeSet<(u16, Protocol)>,
}

impl<'c, 'a> PortPick<'c, 'a> {
    /// A pick for `cx`'s run, nothing picked yet.
    pub(super) fn new(cx: &'c Cx<'a>) -> PortPick<'c, 'a> {
        PortPick {
            cx,
            fixed: cx.vars.params().contains_key(isoloom_manifest::contract::PUBLISH_FIXED),
            held: Vec::new(),
            taken: BTreeSet::new(),
        }
    }

    /// The host port to pin `what` (a published port, as the error names it) to over `protocol`:
    /// its `preferred` number when `ISOLOOM_PUBLISH_FIXED` asks for it and it isn't reserved by
    /// another run (whether the host has it bound is the free-port check's to say), else a free
    /// loopback port.
    ///
    /// # Errors
    ///
    /// `tool_failed` for `step` when no free host port another run starting hasn't reserved is
    /// found (on this computer: every port the OS offered was reserved by another run of this
    /// runner, or held on the IPv6 loopback): the run would otherwise start on ports nobody pinned,
    /// and a resume would get new addresses. `io` when the loopback couldn't be bound to find one
    /// (no file descriptor left, a loopback that is down).
    pub(super) fn pin(&mut self, step: &str, what: &str, preferred: Option<u16>, protocol: Protocol) -> Result<u16> {
        if let Some(port) = preferred.filter(|_| self.fixed).and_then(|n| self.fixed(n, protocol)) {
            return Ok(port);
        }
        let over = crate::encoding::wire_name(&protocol);
        let free = self.free(protocol).map_err(Error::io(format!("a {over} port on 127.0.0.1"), "bound"))?;
        free.ok_or_else(|| Error::ToolFailed {
            step: step.to_owned(),
            reason: format!("no free host port over {over} that another run starting hasn't reserved could be found to pin {what} to"),
        })
    }

    /// `preferred` over `protocol`, when this pick hasn't taken it and no other run reserved it:
    /// reserved for this run.
    fn fixed(&mut self, preferred: u16, protocol: Protocol) -> Option<u16> {
        let free = !self.taken.contains(&(preferred, protocol)) && self.cx.run.runner.reserve(self.cx.run.name(), preferred, protocol);
        free.then(|| {
            self.taken.insert((preferred, protocol));
            preferred
        })
    }

    /// A free loopback port over `protocol`, reserved for this run and held until the pick is
    /// dropped; none when none is left.
    fn free(&mut self, protocol: Protocol) -> std::io::Result<Option<u16>> {
        let Some(port) = self.cx.run.runner.pick_reserved(self.cx.run.name(), protocol)? else {
            return Ok(None);
        };
        let number = port.port;
        self.taken.insert((number, protocol));
        self.held.push(port);
        Ok(Some(number))
    }
}
