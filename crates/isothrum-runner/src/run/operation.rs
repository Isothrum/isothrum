//! What every operation does around its own work: its events numbered, masked and logged, the run
//! locked (failing closed), the op marked in flight, the state moves emitted, the record updated,
//! and exactly one `result` or `error` at the end.

use crate::Result;
use tracing::Instrument;

use crate::clock::rfc3339;
use crate::error::Error;
use crate::events::{Emitter, EventKind, Events};
use crate::run::Running;
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
    /// Runs `work`, the body of `op`, with the run locked and its events going to `events`;
    /// `mask` lists the values no event may show.
    pub(crate) async fn operate<T>(
        &self,
        op: Op,
        events: &dyn Events,
        mask: Vec<String>,
        work: impl AsyncFnOnce(&Emitter<'_>) -> Result<Done<T>>,
    ) -> Result<T> {
        let runner = self.runner;
        let emitter = Emitter::new(self.name().as_str(), op, runner.config.clock.as_ref(), events).masking(mask);
        let emitter = match runner.store.event_log(self.name()) {
            Ok(log) => emitter.with_log(log),
            Err(e) => {
                emitter.error(&e);
                return Err(e);
            }
        };
        let span = tracing::info_span!("run", run = %self.name(), op = %op);
        let done = self.locked(op, &emitter, work).instrument(span.clone()).await;
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

    async fn locked<T>(&self, op: Op, emitter: &Emitter<'_>, work: impl AsyncFnOnce(&Emitter<'_>) -> Result<Done<T>>) -> Result<Done<T>> {
        let store = &self.runner.store;
        let _lock = self.runner.locks.acquire(self.name(), &store.lock_path(self.name())).await?;
        // Mark the op in flight and reserve its ports only now that the run is locked: entered
        // before the lock (R1), a second op on the same run clobbered this entry, and this guard,
        // dropping after the lock was released, freed the second op's port reservations.
        let _flight = self.runner.enter(self.name(), op);
        let before = store.record(self.name())?.map_or(RunState::Absent, |r| r.state);
        if let Some(during) = op.during() {
            emitter.emit(EventKind::State { from: before, to: during });
        }
        let done = work(emitter).await;
        let code = done.as_ref().err().map(Error::code);
        let after = self.close_record(op, code, done.as_ref().ok().and_then(|d| d.state))?;
        if let Some(during) = op.during() {
            emitter.emit(EventKind::State {
                from: during,
                to: after.unwrap_or(before),
            });
        }
        done
    }

    /// Records how `op` ended and the state it left the run in: `state` when it succeeded; when
    /// it failed after it began acting (the record shows a moving state), `failed`. Returns the
    /// recorded state.
    fn close_record(&self, op: Op, code: Option<crate::ErrorCode>, state: Option<RunState>) -> Result<Option<RunState>> {
        let store = &self.runner.store;
        let Some(mut record) = store.record(self.name())? else {
            return Ok(state);
        };
        let moving = matches!(record.state, RunState::Starting | RunState::Stopping | RunState::Parking | RunState::Resuming);
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
