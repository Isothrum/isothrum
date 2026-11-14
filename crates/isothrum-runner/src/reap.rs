//! The reaper's half in the runner, so an abandoned run can't keep billing: [`Runner::expired`],
//! the recorded runs whose lifetime ([`crate::RunOptions::lifetime`], recorded at `up` as the
//! record's `expires_at`) passed and that may still hold something. Removing one is its `down`
//! ([`Runner::reopen`], then [`crate::Running::down`]), which acts on what the run recorded.
//!
//! The sweep's schedule is the embedder's (every few minutes, say; the `isothrum reap` command
//! sweeps once).

use std::time::SystemTime;

use crate::Result;
use crate::clock::unix_seconds;
use crate::runner::Runner;
use crate::state::RunState;
use crate::store::RunRecord;

impl Runner {
    /// The recorded runs whose expiry passed at `now` and whose last operation didn't leave them
    /// absent: the reaper's input. Reads the store only.
    ///
    /// # Errors
    ///
    /// Fails as the store does when a record can't be read.
    pub fn expired(&self, now: SystemTime) -> Result<Vec<RunRecord>> {
        let now = unix_seconds(now);
        let records = self.store.records()?;
        Ok(records
            .into_iter()
            .filter(|r| r.state != RunState::Absent && r.expires_at.is_some_and(|at| at <= now))
            .collect())
    }
}

#[cfg(test)]
mod tests;
