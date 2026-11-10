//! One fresh start's pick of the host ports it pins, shared by the drivers that publish on this
//! computer (Compose, Vagrant): a port's `preferred` number when `ISOLOOM_PUBLISH_FIXED` asks for
//! it, else a free loopback port. Each pick is reserved in the runner until the start ends, so
//! another run of this runner starting meanwhile can't take it, and each free port's socket is
//! held until the whole pick is done, so the OS doesn't hand one out twice.

use std::collections::BTreeSet;

use isoloom_manifest::Protocol;

use crate::executor::Cx;
use crate::net::HeldPort;

/// The ports picked so far, with the sockets holding the free ones.
pub(super) struct PortPick<'c, 'a> {
    cx: &'c Cx<'a>,
    held: Vec<HeldPort>,
    taken: BTreeSet<(u16, Protocol)>,
}

impl<'c, 'a> PortPick<'c, 'a> {
    /// A pick for `cx`'s run, nothing picked yet.
    pub(super) fn new(cx: &'c Cx<'a>) -> PortPick<'c, 'a> {
        PortPick {
            cx,
            held: Vec::new(),
            taken: BTreeSet::new(),
        }
    }

    /// `preferred` over `protocol`, when this pick hasn't taken it and it is free to reserve for
    /// this run.
    pub(super) fn fixed(&mut self, preferred: u16, protocol: Protocol) -> Option<u16> {
        let free = !self.taken.contains(&(preferred, protocol)) && self.cx.run.runner.reserve(self.cx.run.name(), preferred, protocol);
        free.then(|| {
            self.taken.insert((preferred, protocol));
            preferred
        })
    }

    /// A free loopback port over `protocol`, reserved for this run and held until the pick is
    /// dropped.
    pub(super) fn free(&mut self, protocol: Protocol) -> Option<u16> {
        let port = self.cx.run.runner.pick_reserved(self.cx.run.name(), protocol)?;
        let number = port.port;
        self.taken.insert((number, protocol));
        self.held.push(port);
        Some(number)
    }
}
