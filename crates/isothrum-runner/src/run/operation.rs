//! What every operation does around its own work: its events numbered, masked and logged, the run
//! locked (failing closed) and its log trimmed, the op marked in flight, the state moves emitted, the record updated,
//! and exactly one `result` or `error` at the end.

use crate::Result;
use tracing::Instrument;

use crate::clock::rfc3339;
use crate::error::{Error, ErrorCode};
use crate::events::{Emitter, EventKind, Events, WarningCode};
use crate::options::RunOptions;
use crate::run::Running;
use crate::secrets::sensitive_values;
use crate::state::{Op, RunState};
use crate::store::{LastOp, RunRecord};

/// What an operation's own work produced.
pub(crate) struct Done<T> {
    /// What the operation returns.
    pub(crate) value: T,
    /// Whether it reached its goal (a check can fail while the operation succeeds).
    pub(crate) ok: bool,
    /// The `result` event's data.
    pub(crate) data: serde_json::Value,
    /// Where the run stands after it, when the operation moves it.
    pub(crate) state: Option<RunState>,
}

impl Running<'_> {
    /// Runs `work`, the body of `op`, with the run locked and its events going to `events`. `work`
    /// gets `opts` with the run's connection values ([`Running::connected`]); no event shows a
    /// sensitive value of the manifest or of those secrets.
    pub(crate) async fn operate<T>(
        &self,
        op: Op,
        opts: &RunOptions,
        events: &dyn Events,
        work: impl AsyncFnOnce(&RunOptions, &Emitter<'_>) -> Result<Done<T>>,
    ) -> Result<T> {
        let runner = self.runner;
        let opts = self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        let emitter = Emitter::new(self.name().as_str(), op, runner.config.clock.as_ref(), events).masking(mask);
        let emitter = match runner.store.event_log(self.name()) {
            Ok(log) => emitter.with_log(log),
            Err(e) => {
                emitter.error(&e);
                return Err(e);
            }
        };
        let span = tracing::info_span!("run", run = %self.name(), op = %op);
        let cut_short = CutShort::new(&emitter);
        let done = self.locked(op, &opts, &emitter, work).instrument(span.clone()).await;
        cut_short.disarm();
        span.in_scope(|| match &done {
            Ok(d) => tracing::info!(ok = d.ok, "operation done"),
            Err(e) => tracing::warn!(code = %e.code(), error = %e, "operation failed"),
        });
        match &done {
            Ok(d) => emitter.emit(EventKind::Result {
                ok: d.ok,
                data: d.data.clone(),
            }),
            Err(e) => emitter.error(e),
        }
        done.map(|d| d.value)
    }

    async fn locked<T>(
        &self,
        op: Op,
        opts: &RunOptions,
        emitter: &Emitter<'_>,
        work: impl AsyncFnOnce(&RunOptions, &Emitter<'_>) -> Result<Done<T>>,
    ) -> Result<Done<T>> {
        let store = &self.runner.store;
        let _lock = self.runner.locks.acquire(self.name(), &store.lock_path(self.name())).await?;
        // Trim the run's log only now that the run is locked: trimmed before, it was replaced
        // under the operation holding the lock, whose later events went to the file it replaced.
        emitter.reopen_log(|| {
            store.trim_event_log(self.name())?;
            store.event_log(self.name())
        })?;
        // Mark the op in flight and reserve its ports only now that the run is locked: entered
        // before the lock, a second op on the same run clobbered this entry, and this guard,
        // dropping after the lock was released, freed the second op's port reservations.
        let _flight = self.runner.enter(self.name(), op);
        let before = store.record(self.name())?.map_or(RunState::Absent, |r| r.state);
        if let Some(during) = op.during() {
            emitter.emit(EventKind::State { from: before, to: during });
        }
        let unfinished = Unfinished {
            running: self,
            op,
            before,
            emitter,
            armed: true,
        };
        let done = work(opts, emitter).await;
        let code = done.as_ref().err().map(Error::code);
        let settled = unfinished.settle(code, done.as_ref().ok().and_then(|d| d.state), |e| {
            // The work's own failure is the operation's: the record's is said beside it.
            if done.is_ok() {
                return Err(e);
            }
            tracing::warn!(error = %e, "the run's record wasn't updated");
            emitter.warning(WarningCode::RecordNotSaved, &format!("the run's record wasn't updated: {e}"));
            Ok(())
        });
        unfinished.disarm();
        settled?;
        done
    }

    /// Records how `op` ended and the state it left the run in: `state` when it succeeded; when
    /// it failed after it began acting (the record shows a moving state), `failed`. Returns the
    /// recorded state.
    fn close_record(&self, op: Op, code: Option<ErrorCode>, state: Option<RunState>) -> Result<Option<RunState>> {
        let store = &self.runner.store;
        let Some(mut record) = store.record(self.name())? else {
            return Ok(state);
        };
        let moving = record.state.is_moving();
        match (state, code) {
            (Some(state), _) => record.state = state,
            (None, Some(_)) if moving => record.state = RunState::Failed,
            _ => {}
        }
        record.last = Some(LastOp {
            op,
            ok: code.is_none(),
            code,
            at: rfc3339(self.runner.config.clock.now()),
        });
        store.save(&record)?;
        Ok(Some(record.state))
    }

    /// Records that `op` began acting on the run (the record moves to the operation's state).
    pub(crate) fn begin(&self, record: &mut RunRecord, op: Op) -> Result<()> {
        if let Some(during) = op.during() {
            record.state = during;
        }
        self.runner.store.save(record)
    }

    /// Records in `record` that the run now runs from this manifest: its file, which a later
    /// operation that has only the record (a sweep's `down`) reopens, and the boot digest of each
    /// VM, which the next update compares.
    pub(crate) fn runs_from_this(&self, record: &mut RunRecord) {
        record.manifest = self.path().to_path_buf();
        record.vm_boot_digests = super::boot::digests(self.manifest());
    }

    /// The run's record, or a fresh one for this manifest.
    pub(crate) fn record_or_fresh(&self) -> Result<RunRecord> {
        let fresh = || RunRecord::fresh(self.name().clone(), self.path().to_path_buf(), self.manifest().target, self.manifest().driver);
        Ok(self.runner.store.record(self.name())?.unwrap_or_else(fresh))
    }
}

/// An operation in flight: dropped before it ended (its future cut short: an embedder's cancel,
/// the command line's Ctrl-C), it ends the event stream with an `error` (`cancelled`), so an
/// operation still ends with exactly one `result` or `error`.
struct CutShort<'e> {
    emitter: &'e Emitter<'e>,
    armed: bool,
}

impl<'e> CutShort<'e> {
    fn new(emitter: &'e Emitter<'e>) -> CutShort<'e> {
        CutShort { emitter, armed: true }
    }

    /// The operation ended: its own `result` or `error` follows.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for CutShort<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.emitter.emit(EventKind::Error {
                code: ErrorCode::Cancelled,
                message: "the operation was cancelled; the tools it started were stopped".to_owned(),
                step: None,
                timeout: None,
            });
        }
    }
}

/// An operation's work on a locked run: dropped before it ended, the run's record says so (a
/// run caught moving is `failed`, so the next `up` clears what it left), while the run is still
/// locked.
struct Unfinished<'r, 'e> {
    running: &'r Running<'r>,
    op: Op,
    before: RunState,
    emitter: &'e Emitter<'e>,
    armed: bool,
}

impl Unfinished<'_, '_> {
    /// Records how the operation ended ([`Running::close_record`]: failed with `code`, else in
    /// `state`) and emits the state it left the run in. A record that can't be written is what
    /// `unsaved` makes of it: an error stops there, nothing emitted; else the run is said to be
    /// where it was before.
    fn settle(&self, code: Option<ErrorCode>, state: Option<RunState>, unsaved: impl FnOnce(Error) -> Result<()>) -> Result<()> {
        let after = match self.running.close_record(self.op, code, state) {
            Ok(after) => after,
            Err(e) => {
                unsaved(e)?;
                None
            }
        };
        if let Some(during) = self.op.during() {
            self.emitter.emit(EventKind::State {
                from: during,
                to: after.unwrap_or(self.before),
            });
        }
        Ok(())
    }

    /// The work ended: the operation records how.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for Unfinished<'_, '_> {
    /// Cut short: the record says `cancelled`. Nobody is left to report to: a record that can't
    /// be written is traced.
    fn drop(&mut self) {
        if self.armed {
            let traced = |e: Error| {
                tracing::warn!(error = %e, "the record of a cancelled operation wasn't updated");
                Ok(())
            };
            drop(self.settle(Some(ErrorCode::Cancelled), None, traced));
        }
    }
}
