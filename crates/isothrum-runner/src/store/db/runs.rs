//! The runs: what the user wants of each, where each stands, and every move it made.

use rusqlite::{OptionalExtension, Row, params};

use super::{OperationId, RunRow, Tx, failed, from_json, from_word, now, to_json, word};
use crate::Result;
use crate::error::Error;
use crate::name::RunName;
use crate::state::{Desired, RunState, Transition};
use crate::store::{LastObserved, RunRecord};

/// The columns a [`RunRow`] is read from, in [`run_row`]'s order.
const RUN_COLUMNS: &str = "name, manifest_path, manifest_digest, desired, state, failed_cause, state_op, state_at, record, observed, observed_at";

/// The text columns of a run's row, before they are parsed.
struct RawRun {
    name: String,
    manifest: String,
    digest: String,
    desired: String,
    state: String,
    failed_cause: Option<String>,
    state_op: Option<i64>,
    state_at: String,
    record: String,
    observed: Option<String>,
    observed_at: Option<String>,
}

fn raw_run(r: &Row<'_>) -> rusqlite::Result<RawRun> {
    Ok(RawRun {
        name: r.get(0)?,
        manifest: r.get(1)?,
        digest: r.get(2)?,
        desired: r.get(3)?,
        state: r.get(4)?,
        failed_cause: r.get(5)?,
        state_op: r.get(6)?,
        state_at: r.get(7)?,
        record: r.get(8)?,
        observed: r.get(9)?,
        observed_at: r.get(10)?,
    })
}

/// A run's row, parsed; its record's `state` is the row's.
fn run_row(raw: RawRun) -> Result<RunRow> {
    let state: RunState = from_word("state", &raw.state)?;
    let mut record: RunRecord = from_json("record", &raw.record)?;
    record.state = state;
    Ok(RunRow {
        name: RunName::new(&raw.name).map_err(|_| super::corrupt("run name", &raw.name))?,
        manifest: raw.manifest.into(),
        manifest_digest: raw.digest,
        desired: from_word("desired", &raw.desired)?,
        state,
        failed_cause: raw.failed_cause.as_deref().map(|c| from_word("failed_cause", c)).transpose()?,
        state_op: raw.state_op.map(OperationId),
        state_at: raw.state_at,
        record,
        observed: raw.observed.as_deref().map(|o| from_json::<LastObserved>("observation", o)).transpose()?,
        observed_at: raw.observed_at,
    })
}

impl Tx<'_> {
    /// Run `name`'s row, when it has one.
    ///
    /// # Errors
    ///
    /// The store can't be read, or the row holds what this runner doesn't write.
    pub fn run(&self, name: &RunName) -> Result<Option<RunRow>> {
        let sql = format!("SELECT {RUN_COLUMNS} FROM runs WHERE name = ?1");
        let raw = self.tx.query_row(&sql, [name.as_str()], raw_run).optional().map_err(failed("read"))?;
        raw.map(run_row).transpose()
    }

    /// Every run's row, by name.
    ///
    /// # Errors
    ///
    /// As [`Tx::run`].
    pub fn runs(&self) -> Result<Vec<RunRow>> {
        let sql = format!("SELECT {RUN_COLUMNS} FROM runs ORDER BY name");
        let mut stmt = self.tx.prepare(&sql).map_err(failed("read"))?;
        let found = stmt.query_map([], raw_run).map_err(failed("read"))?;
        let mut rows = Vec::new();
        for raw in found {
            rows.push(run_row(raw.map_err(failed("read"))?)?);
        }
        Ok(rows)
    }

    /// Records `record`: a new run's row is made in the record's state (`absent` for a run
    /// never started; another for an imported one), wanting `desired`; an existing row gets the
    /// record's fields and keeps its state, which only [`Tx::apply`] moves.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn save_record(&mut self, record: &RunRecord, desired: Desired) -> Result<()> {
        let json = to_json(record)?;
        let manifest = record.manifest.to_string_lossy().into_owned();
        self.tx
            .execute(
                "INSERT INTO runs (name, manifest_path, manifest_digest, target, driver, desired, state, state_at, record)
                 VALUES (?1, ?2, '', ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(name) DO UPDATE SET manifest_path = excluded.manifest_path, target = excluded.target,
                   driver = excluded.driver, record = excluded.record",
                params![
                    record.name.as_str(),
                    manifest,
                    word(&record.target),
                    word(&record.driver),
                    word(&desired),
                    word(&record.state),
                    now(),
                    json
                ],
            )
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Records the digest of the manifest run `name` now runs from.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn set_manifest_digest(&mut self, name: &RunName, digest: &str) -> Result<()> {
        self.tx
            .execute("UPDATE runs SET manifest_digest = ?2 WHERE name = ?1", params![name.as_str(), digest])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Sets what the user wants of run `name`.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn set_desired(&mut self, name: &RunName, desired: Desired) -> Result<()> {
        self.tx
            .execute("UPDATE runs SET desired = ?2 WHERE name = ?1", params![name.as_str(), word(&desired)])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Moves run `name` by `transition`, and keeps the move with what made it. A compare-and-set:
    /// refused (`state_conflict`) when the run no longer stands where the transition starts.
    ///
    /// # Errors
    ///
    /// `state_conflict` when the run moved meanwhile or has no row; the store's.
    pub fn apply(&mut self, name: &RunName, transition: Transition) -> Result<()> {
        let at = now();
        let op = transition.by().operation().map(|id| id.0);
        let cause = transition.cause().map(|c| word(&c));
        let changed = self
            .tx
            .execute(
                "UPDATE runs SET state = ?3, failed_cause = ?4, state_op = ?5, state_at = ?6 WHERE name = ?1 AND state = ?2",
                params![name.as_str(), word(&transition.from()), word(&transition.to()), cause, op, at],
            )
            .map_err(failed("written"))?;
        if changed == 0 {
            let found = self.run(name)?.map(|r| r.state);
            return Err(Error::StateConflict {
                run: name.to_string(),
                expected: transition.from(),
                found,
            });
        }
        self.tx
            .execute(
                "INSERT INTO transitions (run, from_state, to_state, cause, operation, by, at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    name.as_str(),
                    word(&transition.from()),
                    word(&transition.to()),
                    cause,
                    op,
                    super::to_json(&transition.by())?,
                    at
                ],
            )
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Run `name`'s moves, oldest first: from, to, the operation that made each (none for an
    /// observation).
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn transitions(&self, name: &RunName) -> Result<Vec<(RunState, RunState, Option<OperationId>)>> {
        let mut stmt = self
            .tx
            .prepare("SELECT from_state, to_state, operation FROM transitions WHERE run = ?1 ORDER BY id")
            .map_err(failed("read"))?;
        let rows = stmt
            .query_map([name.as_str()], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Option<i64>>(2)?))
            })
            .map_err(failed("read"))?;
        let mut moves = Vec::new();
        for row in rows {
            let (from, to, op) = row.map_err(failed("read"))?;
            moves.push((from_word("state", &from)?, from_word("state", &to)?, op.map(OperationId)));
        }
        Ok(moves)
    }

    /// Keeps `observed` as what the runner last saw of run `name` (no row: nothing kept), its
    /// state left as it is: an operation's own reads.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn keep_observed(&mut self, name: &RunName, observed: &LastObserved) -> Result<bool> {
        let changed = self
            .tx
            .execute(
                "UPDATE runs SET observed = ?2, observed_at = ?3 WHERE name = ?1",
                params![name.as_str(), to_json(observed)?, now()],
            )
            .map_err(failed("written"))?;
        Ok(changed > 0)
    }

    /// Status = reconcile(desired, observed): keeps `observed`, and when a settled run shows as
    /// `to` (what its machines say), moves it there by an observation. A moving run (an
    /// operation owns it) or a move the table doesn't allow keeps its state. The move made, if
    /// one was.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn observe(&mut self, name: &RunName, observed: &LastObserved, to: RunState) -> Result<Option<Transition>> {
        if !self.keep_observed(name, observed)? {
            return Ok(None);
        }
        let Some(row) = self.run(name)? else {
            return Ok(None);
        };
        if row.state == to {
            return Ok(None);
        }
        let Ok(transition) = Transition::observed(row.state, to) else {
            return Ok(None);
        };
        self.apply(name, transition)?;
        Ok(Some(transition))
    }

    /// Forgets what the runner last saw of run `name`.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn forget_observed(&mut self, name: &RunName) -> Result<()> {
        self.tx
            .execute("UPDATE runs SET observed = NULL, observed_at = NULL WHERE name = ?1", [name.as_str()])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Removes run `name`'s row and its moves.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn forget_run(&mut self, name: &RunName) -> Result<()> {
        self.tx
            .execute("DELETE FROM runs WHERE name = ?1", [name.as_str()])
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Puts run `name` in `state` with no move recorded, as a process killed mid-operation leaves
    /// it: for the tests only, which stand in for that process.
    #[cfg(test)]
    pub(crate) fn force_state(&mut self, name: &RunName, state: RunState) -> Result<()> {
        self.tx
            .execute(
                "UPDATE runs SET state = ?2, failed_cause = NULL WHERE name = ?1",
                params![name.as_str(), word(&state)],
            )
            .map_err(failed("written"))?;
        Ok(())
    }
}
