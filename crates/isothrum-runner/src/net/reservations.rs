//! The host ports the runs of one home hold while they start: a port one run reserved is
//! refused to another starting meanwhile, before either binds it, in this runner or in another
//! process of the home. Each reservation is the run's in this runner, and an OS lock on the
//! port's file under the home's `ports/` (`tcp-8080.lock`), held until the run's operation ends:
//! a start in another process (an embedder runs each lab's start in a worker of its own) finds
//! it locked and moves to another port. The OS releases a dead holder's lock, so a start that
//! crashed never strands a port.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use isoloom_manifest::Protocol;

use super::{HeldPort, HostPorts};
use crate::Result;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::name::RunName;
use crate::store::{FileLock, lock_held, try_hold};

/// A port's reservation: the run that holds it, and the port's lock file it holds for the home.
#[derive(Debug)]
struct Reservation {
    run: RunName,
    _across: FileLock,
}

/// The ports reserved, each by the run that holds it, over the host's ports and the home's
/// `ports/` folder.
pub(crate) struct PortReservations {
    held: Mutex<BTreeMap<(u16, Protocol), Reservation>>,
    ports: Arc<dyn HostPorts>,
    dir: PathBuf,
}

impl PortReservations {
    /// No reservation yet, over `ports`, the home's reservations kept in `dir` (made already).
    pub(crate) fn new(ports: Arc<dyn HostPorts>, dir: PathBuf) -> PortReservations {
        PortReservations {
            held: Mutex::default(),
            ports,
            dir,
        }
    }

    /// The lock file of `port` over `protocol`.
    fn lock_path(&self, port: u16, protocol: Protocol) -> PathBuf {
        self.dir.join(format!("{}-{port}.lock", wire_name(&protocol)))
    }

    /// Reserves `port` for `name` while it starts; false when another run holds it, in this runner
    /// or another process of the home. A lock file that can't be taken refuses the port (fails
    /// closed): the pick moves to another one.
    pub(crate) fn reserve(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        if let Some(holder) = self.holder(port, protocol) {
            return holder == *name;
        }
        // The file lock is taken out of the map's mutex: it arbitrates between two runs of this
        // runner as between two processes, each handle its own.
        let path = self.lock_path(port, protocol);
        let across = match try_hold(&path) {
            Ok(Some(across)) => across,
            Ok(None) => return false,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "a port's reservation file couldn't be locked; the port is refused");
                return false;
            }
        };
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let reservation = held.entry((port, protocol)).or_insert_with(|| Reservation {
            run: name.clone(),
            _across: across,
        });
        reservation.run == *name
    }

    /// The run of this runner that holds `port`, if any.
    fn holder(&self, port: u16, protocol: Protocol) -> Option<RunName> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(port, protocol))
            .map(|r| r.run.clone())
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

    /// Whether another run than `name` holds `port`: one of this runner, or a start in another
    /// process of the home. A lock file that can't be read reads as held (fails closed).
    pub(crate) fn reserved_elsewhere(&self, name: &RunName, port: u16, protocol: Protocol) -> bool {
        if let Some(holder) = self.holder(port, protocol) {
            return holder != *name;
        }
        held_across(&self.lock_path(port, protocol))
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
        self.held.lock().unwrap_or_else(PoisonError::into_inner).retain(|_, r| r.run != *name);
    }
}

/// Whether another process holds the reservation whose lock file is `path`.
fn held_across(path: &Path) -> bool {
    lock_held(path).unwrap_or_else(|e| {
        tracing::warn!(error = %e, path = %path.display(), "a port's reservation file couldn't be read; the port reads as reserved");
        true
    })
}

impl std::fmt::Debug for PortReservations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("PortReservations").field("held", &held.len()).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "reservations_tests.rs"]
mod tests;
