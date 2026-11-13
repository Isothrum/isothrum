//! The host ports the runs of one runner hold while they start: a port one run reserved is
//! refused to another starting meanwhile, before either binds it. Its own type, out of the
//! runner (Clean Code ch.10, one responsibility), and the place a
//! cross-process reservation goes when it comes.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::Protocol;

use super::{HeldPort, HostPorts};
use crate::Result;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::name::RunName;

/// The ports reserved, each by the run that holds it, over the host's ports.
pub(crate) struct PortReservations {
    held: Mutex<BTreeMap<(u16, Protocol), RunName>>,
    ports: Arc<dyn HostPorts>,
}

impl PortReservations {
    /// No reservation yet, over `ports`.
    pub(crate) fn new(ports: Arc<dyn HostPorts>) -> PortReservations {
        PortReservations { held: Mutex::default(), ports }
    }

    /// Reserves `port` for `name` while it starts; false when another run holds it.
    pub(crate) fn reserve(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let holder = held.entry((port, protocol)).or_insert_with(|| name.clone());
        holder == name
    }

    /// A free loopback port over `protocol` that no run holds, reserved for `name` until its
    /// operation ends. The returned socket keeps it bound until dropped, so a port is never
    /// handed out twice; a port another run holds is refused and held meanwhile so it isn't
    /// offered again. None when none is left.
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_reserved(&self, name: &RunName, protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        self.pick_where(protocol, |port| self.reserve(name, port, protocol))
    }

    /// A free loopback port over `protocol` that no run but `name` holds, not reserved: for a
    /// query, which holds no reservation past its end (a console's tunnel, whose `ssh` binds it
    /// at once). None when none is left.
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_unreserved(&self, name: &RunName, protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        self.pick_where(protocol, |port| !self.reserved_elsewhere(name, port, protocol))
    }

    /// A free loopback port over `protocol` that `take` accepts; one it refuses is held meanwhile
    /// so it isn't offered again. None when none is left.
    fn pick_where(&self, protocol: Protocol, take: impl Fn(u16) -> bool) -> std::io::Result<Option<HeldPort>> {
        let mut refused = Vec::new();
        for _ in 0..64 {
            let Some(port) = self.ports.pick_free(protocol)? else {
                return Ok(None);
            };
            if take(port.port) {
                return Ok(Some(port));
            }
            refused.push(port);
        }
        Ok(None)
    }

    /// Whether another run than `name` holds `port`.
    pub(crate) fn reserved_elsewhere(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(port, protocol))
            .is_some_and(|h| h != name)
    }

    /// Each `ports` entry (a host port and its protocol) must be free before run `name` starts:
    /// not reserved by another run starting meanwhile, and not already bound on this computer.
    ///
    /// # Errors
    ///
    /// Fails with `port_in_use` at the first port that is reserved elsewhere or already bound.
    pub(crate) fn ensure_free(&self, name: &RunName, ports: impl IntoIterator<Item = (u16, Protocol)>) -> Result<()> {
        for (port, protocol) in ports {
            if self.reserved_elsewhere(name, port, protocol) || self.ports.in_use(port, protocol) {
                return Err(Error::PortInUse {
                    port,
                    protocol: wire_name(&protocol),
                });
            }
        }
        Ok(())
    }

    /// Frees every port `name` reserved.
    pub(crate) fn release(&self, name: &RunName) {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).retain(|_, holder| holder != name);
    }
}

impl std::fmt::Debug for PortReservations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("PortReservations").field("held", &held.len()).finish_non_exhaustive()
    }
}
