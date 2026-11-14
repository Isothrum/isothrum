//! Leases (a scope held by a process until it lets go, dies or stops renewing) and the host
//! ports reserved under them.

use std::time::Duration;

use crate::project::platform::Protocol;
use rusqlite::{OptionalExtension, params};

use super::{Lease, LeaseOutcome, OperationId, Owner, PortReservation, Tx, epoch_now, failed, from_word, now, word};
use crate::Result;
use crate::name::RunName;

/// Who reserves a port: a run, its operation when journaled, and the lease (its scope, and its
/// owner) the reservation is held under.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Reserver<'a> {
    /// The run.
    pub run: &'a RunName,
    /// Its operation, when journaled.
    pub by: Option<OperationId>,
    /// The lease's scope (`run:<name>`).
    pub lease: &'a str,
    /// The lease's owner.
    pub owner: &'a Owner,
}

impl<'a> Reserver<'a> {
    /// `run`, holding `owner`'s lease `lease`, for its operation `by`.
    #[must_use]
    pub fn new(run: &'a RunName, by: Option<OperationId>, lease: &'a str, owner: &'a Owner) -> Reserver<'a> {
        Reserver { run, by, lease, owner }
    }
}

impl Lease {
    /// Whether it still holds: not expired, and its owner's process may still run.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.expires_at > epoch_now() && self.owner.is_alive()
    }
}

impl Tx<'_> {
    /// Lease `scope`, when one is written (live or not).
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn lease(&self, scope: &str) -> Result<Option<Lease>> {
        let row = self
            .tx
            .query_row(
                "SELECT owner_pid, owner_boot, owner_token, acquired_at, expires_at FROM leases WHERE scope = ?1",
                [scope],
                |r| {
                    Ok(Lease {
                        scope: scope.to_owned(),
                        owner: Owner {
                            pid: r.get(0)?,
                            boot: r.get(1)?,
                            token: r.get(2)?,
                        },
                        acquired_at: r.get(3)?,
                        // SQLite's integers are i64: rusqlite reads no u64.
                        expires_at: r
                            .get::<_, i64>(4)?
                            .try_into()
                            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Integer, Box::new(e)))?,
                    })
                },
            )
            .optional()
            .map_err(failed("read"))?;
        Ok(row)
    }

    /// Takes lease `scope` for `owner` for `ttl`: when nobody holds it, when its holder's lease
    /// expired or its process is gone (taken over: the lease it left is returned), or when
    /// `owner` holds it already (renewed). A live holder keeps it.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn acquire_lease(&mut self, scope: &str, owner: &Owner, ttl: Duration) -> Result<LeaseOutcome> {
        let took_over = match self.lease(scope)? {
            Some(held) if held.owner.token == owner.token => None,
            Some(held) if held.is_live() => return Ok(LeaseOutcome::Held(held)),
            other => other,
        };
        if let Some(dead) = &took_over {
            tracing::info!(scope, pid = dead.owner.pid, "a lease its holder left (gone, or expired) is taken over");
        }
        self.tx
            .execute(
                "INSERT INTO leases (scope, owner_pid, owner_boot, owner_token, acquired_at, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(scope) DO UPDATE SET owner_pid = excluded.owner_pid, owner_boot = excluded.owner_boot,
                   owner_token = excluded.owner_token, acquired_at = excluded.acquired_at, expires_at = excluded.expires_at",
                params![scope, owner.pid, owner.boot, owner.token, now(), expiry(ttl)],
            )
            .map_err(failed("written"))?;
        Ok(LeaseOutcome::Acquired { took_over })
    }

    /// Extends `owner`'s lease `scope` by `ttl` from now: false when `owner` doesn't hold it
    /// (another took it over).
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn renew_lease(&mut self, scope: &str, owner: &Owner, ttl: Duration) -> Result<bool> {
        let changed = self
            .tx
            .execute(
                "UPDATE leases SET expires_at = ?3 WHERE scope = ?1 AND owner_token = ?2",
                params![scope, owner.token, expiry(ttl)],
            )
            .map_err(failed("written"))?;
        Ok(changed > 0)
    }

    /// Lets go of `owner`'s lease `scope` (nothing when another holds it), and of the ports
    /// reserved under it.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn release_lease(&mut self, scope: &str, owner: &Owner) -> Result<()> {
        self.tx
            .execute(
                "DELETE FROM port_reservations WHERE lease = ?1 AND owner_token = ?2",
                params![scope, owner.token],
            )
            .map_err(failed("written"))?;
        self.tx
            .execute("DELETE FROM leases WHERE scope = ?1 AND owner_token = ?2", params![scope, owner.token])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Reserves host `port` over `protocol` for `reserver`: false when another run's live
    /// reservation holds it. A reservation whose lease is gone, expired or another's now (its
    /// process died) is taken over.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn reserve_port(&mut self, reserver: &Reserver<'_>, port: u16, protocol: Protocol) -> Result<bool> {
        let Reserver { run, by, lease, owner } = *reserver;
        if let Some(held) = self.port_holder(port, protocol)? {
            return Ok(held.run == *run);
        }
        self.tx
            .execute(
                "INSERT INTO port_reservations (protocol, port, run, operation, lease, owner_token) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(protocol, port) DO UPDATE SET run = excluded.run, operation = excluded.operation,
                   lease = excluded.lease, owner_token = excluded.owner_token",
                params![word(&protocol), port, run.as_str(), by.map(|id| id.0), lease, owner.token],
            )
            .map_err(failed("written"))?;
        Ok(true)
    }

    /// The live reservation of `port` over `protocol`, if one holds it: one whose lease its
    /// reserver still holds, live.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn port_holder(&self, port: u16, protocol: Protocol) -> Result<Option<PortReservation>> {
        let row: Option<(String, Option<i64>, String, String)> = self
            .tx
            .query_row(
                "SELECT run, operation, lease, owner_token FROM port_reservations WHERE protocol = ?1 AND port = ?2",
                params![word(&protocol), port],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(failed("read"))?;
        let Some((run, operation, lease, token)) = row else {
            return Ok(None);
        };
        let live = self.lease(&lease)?.is_some_and(|l| l.owner.token == token && l.is_live());
        if !live {
            return Ok(None);
        }
        Ok(Some(PortReservation {
            port,
            protocol,
            run: RunName::new(&run).map_err(|_| super::corrupt("run name", &run))?,
            operation: operation.map(OperationId),
        }))
    }

    /// Frees every port operation `by` reserved.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn release_ports(&mut self, by: OperationId) -> Result<()> {
        self.tx
            .execute("DELETE FROM port_reservations WHERE operation = ?1", [by.0])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Every live reservation, by port.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn port_reservations(&self) -> Result<Vec<PortReservation>> {
        let mut stmt = self
            .tx
            .prepare("SELECT protocol, port FROM port_reservations ORDER BY protocol, port")
            .map_err(failed("read"))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u16>(1)?)))
            .map_err(failed("read"))?;
        let mut keys = Vec::new();
        for row in rows {
            let (protocol, port) = row.map_err(failed("read"))?;
            keys.push((from_word::<Protocol>("protocol", &protocol)?, port));
        }
        let mut live = Vec::new();
        for (protocol, port) in keys {
            live.extend(self.port_holder(port, protocol)?);
        }
        Ok(live)
    }
}

/// The epoch second `ttl` from now.
fn expiry(ttl: Duration) -> i64 {
    i64::try_from(epoch_now().saturating_add(ttl.as_secs())).unwrap_or(i64::MAX)
}
