//! The journal (each operation: started, its steps, ended) and the event table.

use rusqlite::{OptionalExtension, Row, params};

use super::{Ended, EventCursor, EventPage, OperationId, OperationRow, Outcome, Owner, Tx, failed, from_json, from_word, now, to_json, word};
use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::events::{Event, StepState};
use crate::name::RunName;
use crate::state::Op;

/// The columns an [`OperationRow`] is read from, in [`operation_row`]'s order.
const OPERATION_COLUMNS: &str =
    "id, run, op, queued_at, started_at, ended_at, outcome, error_code, cancel_requested, owner_pid, owner_boot, owner_token, resumed_from";

/// The text columns of an operation's row, before they are parsed.
struct RawOperation {
    id: i64,
    run: String,
    op: String,
    queued_at: String,
    started_at: Option<String>,
    ended_at: Option<String>,
    outcome: Option<String>,
    code: Option<String>,
    cancel: bool,
    pid: u32,
    boot: String,
    token: String,
    resumed_from: Option<i64>,
}

fn raw_operation(r: &Row<'_>) -> rusqlite::Result<RawOperation> {
    Ok(RawOperation {
        id: r.get(0)?,
        run: r.get(1)?,
        op: r.get(2)?,
        queued_at: r.get(3)?,
        started_at: r.get(4)?,
        ended_at: r.get(5)?,
        outcome: r.get(6)?,
        code: r.get(7)?,
        cancel: r.get(8)?,
        pid: r.get(9)?,
        boot: r.get(10)?,
        token: r.get(11)?,
        resumed_from: r.get(12)?,
    })
}

fn operation_row(raw: RawOperation) -> Result<OperationRow> {
    let ended = match (raw.ended_at, raw.outcome) {
        (Some(at), Some(outcome)) => Some(Ended {
            at,
            outcome: from_word("outcome", &outcome)?,
            code: raw.code.as_deref().map(|c| from_word("error_code", c)).transpose()?,
        }),
        _ => None,
    };
    Ok(OperationRow {
        id: OperationId(raw.id),
        run: RunName::new(&raw.run).map_err(|_| super::corrupt("run name", &raw.run))?,
        op: from_word("op", &raw.op)?,
        queued_at: raw.queued_at,
        started_at: raw.started_at,
        ended,
        cancel_requested: raw.cancel,
        owner: Owner {
            pid: raw.pid,
            boot: raw.boot,
            token: raw.token,
        },
        resumed_from: raw.resumed_from.map(OperationId),
    })
}

impl Tx<'_> {
    /// Journals operation `op` on `run` as queued by `owner` (a service answering a command): its
    /// id is the one every later event and the client see; [`Tx::start_operation`] starts it.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn queue_operation(&mut self, run: &RunName, op: Op, owner: &Owner) -> Result<OperationRow> {
        self.tx
            .execute(
                "INSERT INTO operations (run, op, queued_at, owner_pid, owner_boot, owner_token) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![run.as_str(), word(&op), now(), owner.pid, owner.boot, owner.token],
            )
            .map_err(failed("written"))?;
        let id = OperationId(self.tx.last_insert_rowid());
        self.operation(id)?.ok_or_else(|| super::corrupt("operation", "a row just written"))
    }

    /// Starts queued operation `id` of `run`, owned now by `owner`, recovering `resumed_from` when
    /// it found that one unfinished.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when `id` isn't a queued operation of `run` (started or ended
    /// already, another run's, or none); the store's.
    pub fn start_operation(&mut self, id: OperationId, run: &RunName, owner: &Owner, resumed_from: Option<OperationId>) -> Result<OperationRow> {
        let changed = self
            .tx
            .execute(
                "UPDATE operations SET started_at = ?3, owner_pid = ?4, owner_boot = ?5, owner_token = ?6, resumed_from = ?7
                 WHERE id = ?1 AND run = ?2 AND started_at IS NULL AND ended_at IS NULL",
                params![id.0, run.as_str(), now(), owner.pid, owner.boot, owner.token, resumed_from.map(|r| r.0)],
            )
            .map_err(failed("written"))?;
        if changed == 0 {
            return Err(Error::Store {
                action: "written",
                code: ErrorCode::IllegalTransition,
                reason: format!("operation {id} isn't a queued operation of run `{run}`"),
            });
        }
        self.operation(id)?.ok_or_else(|| super::corrupt("operation", "a row just written"))
    }

    /// Journals operation `op` on `run` as queued and started at once by `owner` (the command
    /// line, an embedder with no service), recovering `resumed_from` when it found that one
    /// unfinished.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn begin_operation(&mut self, run: &RunName, op: Op, owner: &Owner, resumed_from: Option<OperationId>) -> Result<OperationRow> {
        let queued = self.queue_operation(run, op, owner)?;
        self.start_operation(queued.id, run, owner, resumed_from)
    }

    /// Journals that step `step` of operation `id` reached `state`.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn step(&mut self, id: OperationId, step: &str, state: StepState) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO operation_steps (operation, seq, step, state, at)
                 VALUES (?1, (SELECT COALESCE(MAX(seq), 0) + 1 FROM operation_steps WHERE operation = ?1), ?2, ?3, ?4)",
                params![id.0, step, word(&state), now()],
            )
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Operation `id`'s journaled steps, in order: each step and where it reached.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn steps(&self, id: OperationId) -> Result<Vec<(String, StepState)>> {
        let mut stmt = self
            .tx
            .prepare("SELECT step, state FROM operation_steps WHERE operation = ?1 ORDER BY seq")
            .map_err(failed("read"))?;
        let rows = stmt
            .query_map([id.0], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(failed("read"))?;
        let mut steps = Vec::new();
        for row in rows {
            let (step, state) = row.map_err(failed("read"))?;
            steps.push((step, from_word("step state", &state)?));
        }
        Ok(steps)
    }

    /// Journals how operation `id` ended. An operation already ended keeps its first end. A
    /// queued one that never started can only be `cancelled`: it never ran.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when a queued operation is ended otherwise than cancelled; the
    /// store's.
    pub fn end_operation(&mut self, id: OperationId, outcome: Outcome, code: Option<ErrorCode>) -> Result<()> {
        if outcome != Outcome::Cancelled && self.operation(id)?.is_some_and(|o| o.started_at.is_none() && o.ended.is_none()) {
            return Err(Error::Store {
                action: "written",
                code: ErrorCode::IllegalTransition,
                reason: format!("operation {id} never started: it can only be cancelled"),
            });
        }
        self.tx
            .execute(
                "UPDATE operations SET ended_at = ?2, outcome = ?3, error_code = ?4 WHERE id = ?1 AND ended_at IS NULL",
                params![id.0, now(), word(&outcome), code.map(|c| word(&c))],
            )
            .map_err(failed("written"))?;
        Ok(())
    }

    /// Asks operation `id` to stop: false when it already ended.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn request_cancel(&mut self, id: OperationId) -> Result<bool> {
        let changed = self
            .tx
            .execute("UPDATE operations SET cancel_requested = 1 WHERE id = ?1 AND ended_at IS NULL", [id.0])
            .map_err(failed("written"))?;
        Ok(changed > 0)
    }

    /// Operation `id`.
    ///
    /// # Errors
    ///
    /// The store can't be read, or the row holds what this runner doesn't write.
    pub fn operation(&self, id: OperationId) -> Result<Option<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE id = ?1");
        let raw = self.tx.query_row(&sql, [id.0], raw_operation).optional().map_err(failed("read"))?;
        raw.map(operation_row).transpose()
    }

    /// Run `run`'s last `limit` operations, newest first.
    ///
    /// # Errors
    ///
    /// As [`Tx::operation`].
    pub fn operations(&self, run: &RunName, limit: usize) -> Result<Vec<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE run = ?1 ORDER BY id DESC LIMIT ?2");
        self.operations_where(&sql, params![run.as_str(), i64::try_from(limit).unwrap_or(i64::MAX)])
    }

    /// The home's operations that never ended, every run's, queued or started, oldest first:
    /// those still in their owner's queue or running while it lives, else left by a process that
    /// died.
    ///
    /// # Errors
    ///
    /// As [`Tx::operation`].
    pub fn in_flight(&self) -> Result<Vec<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE ended_at IS NULL ORDER BY id");
        self.operations_where(&sql, params![])
    }

    /// Run `run`'s operations that never ended, queued or started, oldest first. Called with the
    /// run held (its lock, or its lease), so none of the started ones runs: each is one whose
    /// process died, or whose future was dropped before it could say how it ended. A queued one
    /// still waits in its owner's queue while that owner lives.
    ///
    /// # Errors
    ///
    /// As [`Tx::operation`].
    pub fn unfinished(&self, run: &RunName) -> Result<Vec<OperationRow>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE run = ?1 AND ended_at IS NULL ORDER BY id");
        self.operations_where(&sql, params![run.as_str()])
    }

    /// Ends `cancelled` every queued operation of the home (any run) that never started and whose
    /// owner is gone (another boot, a dead process): a service that died with operations in its
    /// queue. Returns their ids. A live owner's queued rows stay: they wait in its queue.
    ///
    /// # Errors
    ///
    /// The store can't be read or written.
    pub fn cancel_orphaned_queued(&mut self) -> Result<Vec<OperationId>> {
        let sql = format!("SELECT {OPERATION_COLUMNS} FROM operations WHERE started_at IS NULL AND ended_at IS NULL ORDER BY id");
        let orphans: Vec<OperationId> = self
            .operations_where(&sql, params![])?
            .into_iter()
            .filter(|o| !o.owner.is_alive())
            .map(|o| o.id)
            .collect();
        for id in &orphans {
            self.end_operation(*id, Outcome::Cancelled, Some(ErrorCode::Cancelled))?;
        }
        Ok(orphans)
    }

    fn operations_where(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<OperationRow>> {
        let mut stmt = self.tx.prepare(sql).map_err(failed("read"))?;
        let found = stmt.query_map(params, raw_operation).map_err(failed("read"))?;
        let mut rows = Vec::new();
        for raw in found {
            rows.push(operation_row(raw.map_err(failed("read"))?)?);
        }
        Ok(rows)
    }

    /// Appends `event` (of operation `operation`, if any) to the event table: its cursor.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn append_event(&mut self, operation: Option<OperationId>, event: &Event) -> Result<EventCursor> {
        self.append_raw_event(event.run.as_deref(), operation, &to_json(event)?)
    }

    /// Appends an event already written as JSON, as written (another version's kind): it is read
    /// back as an event when this runner reads it, else counted unreadable.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn append_raw_event(&mut self, run: Option<&str>, operation: Option<OperationId>, body: &str) -> Result<EventCursor> {
        self.tx
            .execute(
                "INSERT INTO events (run, operation, body) VALUES (?1, ?2, ?3)",
                params![run, operation.map(|id| id.0), body],
            )
            .map_err(failed("written"))?;
        Ok(EventCursor(u64::try_from(self.tx.last_insert_rowid()).unwrap_or(0)))
    }

    /// Up to `limit` events after cursor `after`, of run `run` or of every run, oldest first,
    /// with how many rows it couldn't read as an event (another version's kind).
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn events_after(&self, after: EventCursor, run: Option<&RunName>, limit: usize) -> Result<(Vec<(EventCursor, Event)>, usize)> {
        let page = self.events_page(after, run, limit)?;
        Ok((page.events, page.unreadable))
    }

    /// Up to `limit` event rows after cursor `after`, of run `run` or of every run, oldest first:
    /// the events it reads, how many rows it couldn't read as one, and the cursor of the last row
    /// it scanned, read or not (`after` when there was none). A reader resumes from `last`: a
    /// page of rows another version wrote, none of them readable, still moves it on.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn events_page(&self, after: EventCursor, run: Option<&RunName>, limit: usize) -> Result<EventPage> {
        let mut stmt = self
            .tx
            .prepare("SELECT cursor, body FROM events WHERE cursor > ?1 AND (?2 IS NULL OR run = ?2) ORDER BY cursor LIMIT ?3")
            .map_err(failed("read"))?;
        let rows = stmt
            .query_map(
                params![
                    i64::try_from(after.0).unwrap_or(i64::MAX),
                    run.map(RunName::as_str),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(failed("read"))?;
        let mut page = EventPage {
            events: Vec::new(),
            unreadable: 0,
            last: after,
        };
        for row in rows {
            let (cursor, body) = row.map_err(failed("read"))?;
            let cursor = EventCursor(u64::try_from(cursor).unwrap_or(0));
            page.last = cursor;
            match from_json::<Event>("event", &body) {
                Ok(event) => page.events.push((cursor, event)),
                Err(e) => {
                    tracing::debug!(cursor = cursor.0, error = %e, "an event row isn't an event this runner reads");
                    page.unreadable += 1;
                }
            }
        }
        Ok(page)
    }

    /// The newest event's cursor (0 when there is none).
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn last_cursor(&self) -> Result<EventCursor> {
        let last: i64 = self
            .tx
            .query_row("SELECT COALESCE(MAX(cursor), 0) FROM events", [], |r| r.get(0))
            .map_err(failed("read"))?;
        Ok(EventCursor(u64::try_from(last).unwrap_or(0)))
    }

    /// Keeps run `run`'s newest `keep` events.
    ///
    /// # Errors
    ///
    /// The store can't be written.
    pub fn trim_events(&mut self, run: &RunName, keep: usize) -> Result<()> {
        self.tx
            .execute(
                "DELETE FROM events WHERE run = ?1 AND cursor NOT IN (SELECT cursor FROM events WHERE run = ?1 ORDER BY cursor DESC LIMIT ?2)",
                params![run.as_str(), i64::try_from(keep).unwrap_or(i64::MAX)],
            )
            .map_err(failed("written"))?;
        Ok(())
    }
}
