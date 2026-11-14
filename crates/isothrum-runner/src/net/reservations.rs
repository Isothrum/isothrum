//! The host ports the runs of one home hold while they start: a port one run reserved is
//! refused to another starting meanwhile, before either binds it, in this runner or in another
//! process of the home. Each reservation is a row of the home's store, held under its run's
//! lease (`run:<name>`), which this runner renews while it holds a port: a process that dies
//! stops renewing it, and its pid is gone, so the next start takes the port over. No start that
//! crashed strands a port.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::Duration;

use isoloom_manifest::Protocol;

use super::{HeldPort, HostPorts};
use crate::Result;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::name::RunName;
use crate::store::{LeaseOutcome, OperationId, Owner, Reserver, Store};

/// How long a run's lease holds unless renewed.
const LEASE_TTL: Duration = Duration::from_secs(60);

/// How often a held lease is renewed.
const RENEW_EVERY: Duration = Duration::from_secs(20);

/// A run's hold on its ports: the lease's owner, the operation reserving them, and the
/// renewal's stop flag.
#[derive(Debug)]
struct Holder {
    owner: Owner,
    operation: Option<OperationId>,
    stop: Arc<AtomicBool>,
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The ports reserved by this runner's runs, over the host's ports and the home's store.
pub(crate) struct PortReservations {
    holders: Mutex<BTreeMap<RunName, Holder>>,
    ports: Arc<dyn HostPorts>,
    db: Arc<Store>,
}

/// The lease a run's reservations are held under.
fn scope(run: &RunName) -> String {
    format!("run:{run}")
}

impl PortReservations {
    /// No reservation yet, over `ports`, kept in `db`.
    pub(crate) fn new(ports: Arc<dyn HostPorts>, db: Arc<Store>) -> PortReservations {
        PortReservations {
            holders: Mutex::default(),
            ports,
            db,
        }
    }

    /// Takes run `name`'s lease for this runner, and renews it while held: None when another live
    /// process holds it, or the store can't be written (fails closed: the port is refused).
    fn hold(&self, name: &RunName) -> Option<Holder> {
        let owner = Owner::this_process();
        let scope = scope(name);
        match self.db.write(|tx| tx.acquire_lease(&scope, &owner, LEASE_TTL)) {
            Ok(LeaseOutcome::Acquired { .. }) => {}
            Ok(LeaseOutcome::Held(lease)) => {
                tracing::warn!(run = %name, pid = lease.owner.pid, "another process holds the run's lease; its ports are refused");
                return None;
            }
            Err(e) => {
                tracing::warn!(run = %name, error = %e, "the run's lease couldn't be taken; the port is refused");
                return None;
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        renew(Arc::downgrade(&self.db), scope, owner.clone(), Arc::clone(&stop));
        Some(Holder { owner, operation: None, stop })
    }

    /// Reserves `port` for `name` while it starts; false when another run holds it, in this
    /// runner or another process of the home. A reservation that can't be written refuses the
    /// port (fails closed): the pick moves to another one.
    pub(crate) fn reserve(&self, name: &RunName, port: crate::port::HostPort, protocol: Protocol, by: Option<OperationId>) -> bool {
        let mut holders = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        if !holders.contains_key(name) {
            let Some(holder) = self.hold(name) else {
                return false;
            };
            holders.insert(name.clone(), holder);
        }
        let Some(holder) = holders.get_mut(name) else {
            return false;
        };
        if by.is_some() {
            holder.operation = by;
        }
        let scope = scope(name);
        let reserved = self
            .db
            .write(|tx| tx.reserve_port(&Reserver::new(name, holder.operation, &scope, &holder.owner), port, protocol));
        reserved.unwrap_or_else(|e| {
            tracing::warn!(error = %e, port = port.get(), "a port's reservation couldn't be written; the port is refused");
            false
        })
    }

    /// A free loopback port over `protocol` that no run holds, reserved for `name` until its
    /// operation ends. The returned socket keeps it bound until dropped, so a port is never
    /// handed out twice; a port another run holds is refused and held meanwhile so it isn't
    /// offered again. None when none is left.
    ///
    /// # Errors
    ///
    /// The loopback couldn't be bound.
    pub(crate) fn pick_reserved(&self, name: &RunName, protocol: Protocol, by: Option<OperationId>) -> std::io::Result<Option<HeldPort>> {
        self.pick_where(protocol, |port| self.reserve(name, port, protocol, by))
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
    fn pick_where(&self, protocol: Protocol, take: impl Fn(crate::port::HostPort) -> bool) -> std::io::Result<Option<HeldPort>> {
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

    /// Whether another run than `name` holds `port`, in this runner or another process of the
    /// home. A store that can't be read reads as held (fails closed).
    pub(crate) fn reserved_elsewhere(&self, name: &RunName, port: crate::port::HostPort, protocol: Protocol) -> bool {
        match self.db.read(|tx| tx.port_holder(port, protocol)) {
            Ok(holder) => holder.is_some_and(|h| h.run != *name),
            Err(e) => {
                tracing::warn!(error = %e, port = port.get(), "a port's reservation couldn't be read; the port reads as reserved");
                true
            }
        }
    }

    /// Each `ports` entry (a host port and its protocol) must be free before run `name` starts:
    /// not reserved by another run starting meanwhile, and not already bound on this computer.
    ///
    /// # Errors
    ///
    /// Fails with `port_in_use` at the first port that is reserved elsewhere or already bound.
    pub(crate) fn ensure_free(&self, name: &RunName, ports: impl IntoIterator<Item = (crate::port::HostPort, Protocol)>) -> Result<()> {
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

    /// Frees every port `name` reserved, with its lease.
    pub(crate) fn release(&self, name: &RunName) {
        let Some(holder) = self.holders.lock().unwrap_or_else(PoisonError::into_inner).remove(name) else {
            return;
        };
        if let Err(e) = self.db.write(|tx| tx.release_lease(&scope(name), &holder.owner)) {
            tracing::warn!(run = %name, error = %e, "the run's ports weren't released; they are freed when its lease expires");
        }
    }
}

/// Renews `owner`'s lease `scope` every [`RENEW_EVERY`] until `stop` is set or the store is
/// gone, on a thread of its own: an operation's async tasks may all be blocked on a tool.
fn renew(db: Weak<Store>, scope: String, owner: Owner, stop: Arc<AtomicBool>) {
    let spawned = std::thread::Builder::new().name("isothrum-lease".to_owned()).spawn(move || {
        loop {
            std::thread::sleep(RENEW_EVERY);
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let Some(db) = db.upgrade() else { return };
            match db.write(|tx| tx.renew_lease(&scope, &owner, LEASE_TTL)) {
                Ok(true) => {}
                Ok(false) => return,
                Err(e) => tracing::warn!(scope, error = %e, "a lease wasn't renewed"),
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "a lease's renewal couldn't be started: it holds for its first minute");
    }
}

impl std::fmt::Debug for PortReservations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let held = self.holders.lock().unwrap_or_else(PoisonError::into_inner);
        f.debug_struct("PortReservations").field("held", &held.len()).finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "reservations_tests.rs"]
mod tests;
