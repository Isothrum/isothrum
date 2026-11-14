//! What every operation does around its own work: its events numbered, masked and logged, the run
//! locked (failing closed) and its log trimmed, the op marked in flight, the state moves emitted, the record updated,
//! and exactly one `result` or `error` at the end.

use crate::Result;
use tracing::Instrument;

use crate::clock::rfc3339;
use crate::error::{Error, ErrorCode};
use crate::events::StoreLog;
use crate::events::{Emitter, EventKind, Events, ResultData, WarningCode};
use crate::options::RunOptions;
use crate::run::Running;
use crate::secrets::sensitive_values;
use crate::state::{Desired, Op, RunState, Transition};
use crate::store::{LastOp, OperationId, Outcome, Owner, RunRecord};
use std::sync::Arc;

/// What an operation's own work produced.
pub(crate) struct Done<T> {
    /// What the operation returns.
    pub(crate) value: T,
    /// Whether it reached its goal (a check can fail while the operation succeeds).
    pub(crate) ok: bool,
    /// The `result` event's data.
    pub(crate) data: ResultData,
    /// Where the run stands after it, when the operation moves it.
    pub(crate) state: Option<RunState>,
}

impl<T> Done<T> {
    /// An operation that reached its goal, returning `value`, its `result` event carrying `data`,
    /// the run left where `state` says (or where it was). One constructor for the seven literals
    /// that spelled `ok: true` by hand (Clean Code G5, 2026-11-13).
    pub(crate) fn settled(value: T, state: Option<RunState>, data: ResultData) -> Done<T> {
        Done { value, ok: true, data, state }
    }

    /// An operation whose checks decide whether it reached its goal: `ok` is the report's, read
    /// before the report moves in.
    pub(crate) fn checked(ok: bool, value: T, data: ResultData) -> Done<T> {
        Done { value, ok, data, state: None }
    }
}

impl Done<()> {
    /// An operation that moved the run to `state` and returns nothing else: its event says the
    /// state.
    pub(crate) fn moved(state: RunState) -> Done<()> {
        Done::settled((), Some(state), ResultData::Moved { state })
    }
}

impl Running<'_> {
    /// Runs `work`, the body of `op`, with the run locked and its events going to `events`. `work`
    /// gets `opts` with the run's connection values ([`Running::with_connection_secrets`]); no event shows a
    /// sensitive value of the manifest or of those secrets.
    pub(crate) async fn operate<T>(
        &self,
        op: Op,
        opts: &RunOptions,
        events: &dyn Events,
        work: impl AsyncFnOnce(&RunOptions, &Emitter<'_>) -> Result<Done<T>>,
    ) -> Result<T> {
        let runner = self.runner;
        let opts = self.with_connection_secrets(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        let emitter = Emitter::new(self.name().as_str(), op, runner.config.clock.as_ref(), events).masking(mask);
        let emitter = emitter.with_log(StoreLog::new(Arc::clone(runner.store.db())));
        let span = tracing::info_span!("run", run = %self.name(), op = %op, operation = tracing::field::Empty);
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
        store.run_dir(self.name())?;
        let _lock = self.runner.locks.acquire(self.name(), &store.lock_path(self.name())).await?;
        store.trim_event_log(self.name())?;
        // Journaled, and marked in flight, only now that the run is locked: no other operation
        // runs on it, so every operation of the run the journal shows unfinished is one whose
        // process died (or whose future was dropped before it could say how it ended).
        let id = self.journal_begin(op, opts.operation)?;
        tracing::Span::current().record("operation", id.0);
        emitter.journal_as(id);
        // Entered after the lock: entered before, a second op on the same run clobbered this
        // entry, and this guard, dropping after the lock was released, freed its port
        // reservations.
        let _flight = self.runner.enter(self.name(), op, id);
        self.end_left_running().await;
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
        let done = work(opts, emitter).await.map_err(|e| self.classify_server_refusal(e, opts));
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

    /// `e` as the server refusing the run's credentials when it is a step's failure in the
    /// server's words for one, on a run on a server ([`crate::server_auth`]), named by the
    /// operation's params, else the ones its record replays.
    pub(crate) fn classify_server_refusal(&self, e: Error, opts: &RunOptions) -> Error {
        if e.code() != ErrorCode::ToolFailed {
            return e;
        }
        let record = self.runner.store.record(self.name()).ok().flatten();
        let provider = opts.provider.or_else(|| record.as_ref().and_then(|r| r.provider));
        if !crate::server_auth::on_server(self.manifest(), provider) {
            return e;
        }
        let mut params = record.as_ref().and_then(RunRecord::replayed_params).cloned().unwrap_or_default();
        params.extend(opts.params.iter().map(|(k, v)| (k.clone(), v.clone())));
        crate::server_auth::refusal(e, crate::server_auth::server_of(&params))
    }

    /// Records how `op` ended, in one transaction: the move it makes the run (to its goal when it
    /// succeeded, `failed` with its code when it failed after it began acting; none when it
    /// never began), the record's last operation, and the journal's end. Returns where the run
    /// stands.
    fn close_record(&self, op: Op, code: Option<ErrorCode>, _state: Option<RunState>) -> Result<Option<RunState>> {
        let name = self.name();
        let begun = self.runner.begun(name);
        let id = self.runner.operation_of(name);
        let at = rfc3339(self.runner.config.clock.now());
        let outcome = match code {
            None => Outcome::Succeeded,
            Some(ErrorCode::Cancelled) => Outcome::Cancelled,
            Some(_) => Outcome::Failed,
        };
        self.runner.store.db().write(|tx| {
            if let Some(id) = id {
                tx.end_operation(id, outcome, code)?;
            }
            let Some(row) = tx.run(name)? else {
                return Ok(None);
            };
            let mut record = row.record;
            record.last = Some(LastOp::ended(op, code, at));
            tx.save_record(&record, row.desired)?;
            let moved = begun.map(|b| match code {
                None => Transition::succeed(b),
                Some(code) => Transition::fail(b, code),
            });
            if let Some(t) = moved {
                tx.apply(name, t)?;
                if code.is_none()
                    && let Some(desired) = Desired::after(op)
                {
                    tx.set_desired(name, desired)?;
                }
            }
            Ok(Some(moved.map_or(row.state, |t| t.to())))
        })
    }

    /// Journals `op` as begun, and settles what operations whose process died left: each
    /// unfinished one of the run is ended `abandoned`, and a run its record shows moving
    /// (`creating`, `destroying`, ...) is recovered to `failed`, `cancelled`, by this operation:
    /// what it left is a failed run's, which the next `up` clears rather than calling it running.
    fn journal_begin(&self, op: Op, queued: Option<OperationId>) -> Result<OperationId> {
        let name = self.name();
        let owner = Owner::this_process();
        self.runner.store.db().write(|tx| {
            let unfinished: Vec<_> = tx.unfinished(name)?.into_iter().filter(|o| Some(o.id) != queued).collect();
            // Started and never ended: its process died. Queued and never started: its owner's
            // queue still holds it while that owner lives; an owner gone, it never ran.
            let (started, waiting): (Vec<_>, Vec<_>) = unfinished.into_iter().partition(|o| o.started_at.is_some());
            let resumed = started.last().map(|o| o.id);
            let row = match queued {
                Some(id) => tx.start_operation(id, name, &owner, resumed)?,
                None => tx.begin_operation(name, op, &owner, resumed)?,
            };
            for left in &started {
                tracing::warn!(operation = %left.id, op = %left.op, "an operation of the run ended without recording how: abandoned");
                tx.end_operation(left.id, Outcome::Abandoned, Some(ErrorCode::Cancelled))?;
            }
            for orphan in waiting.iter().filter(|o| !o.owner.is_alive()) {
                tracing::warn!(operation = %orphan.id, op = %orphan.op, "a queued operation's owner is gone: cancelled");
                tx.end_operation(orphan.id, Outcome::Cancelled, Some(ErrorCode::Cancelled))?;
            }
            if let Some(run) = tx.run(name)?
                && let Ok(recovered) = Transition::cut_off(run.state, row.id)
            {
                tracing::warn!(state = %run.state, "the run's last operation ended without recording how: taken as failed");
                tx.apply(name, recovered)?;
                let mut record = run.record;
                record.last = Some(LastOp::ended(
                    run.state.moved_by().unwrap_or(op),
                    Some(ErrorCode::Cancelled),
                    rfc3339(self.runner.config.clock.now()),
                ));
                tx.save_record(&record, run.desired)?;
            }
            Ok(row.id)
        })
    }

    /// Ends the tools a killed operation on the run left running ([`crate::process::ledger`]):
    /// before anything acts on the run, with it locked, so this operation never races a
    /// `vagrant up` or a `docker compose up` still going from the last.
    async fn end_left_running(&self) {
        let dir = self.runner.store.run_dir_path(self.name()).join(crate::process::ledger::DIR);
        let ended = crate::process::ledger::end_orphans(&dir).await;
        if !ended.is_empty() {
            tracing::warn!(?ended, "ended the tools a killed operation on the run left running");
        }
    }

    /// Records that `op` began acting on the run: `record`'s fields, and the run's move to the
    /// state the operation holds it in, in one transaction. An op that moves no run only saves.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when the run can't begin `op` from where it stands; `state_conflict`
    /// when it moved meanwhile; the store's.
    pub(crate) fn begin(&self, record: &mut RunRecord, op: Op, events: &Emitter<'_>) -> Result<()> {
        let name = self.name();
        let Some(id) = self.runner.operation_of(name) else {
            return self.runner.store.save(record);
        };
        self.runner.store.run_dir(name)?;
        let begun = self.runner.store.db().write(|tx| {
            tx.save_record(record, Desired::Running)?;
            let Some(row) = tx.run(name)? else {
                return Ok(None);
            };
            if op.during().is_none() {
                return Ok(None);
            }
            let begun = Transition::begin(row.state, op, id)?;
            tx.apply(name, begun)?;
            // Said whenever an operation that sets it begins, so a client sees the wish with the
            // operation that carries it out (a new run's row already starts out `running`).
            let wanted = Desired::after(op);
            if let Some(desired) = wanted {
                tx.set_desired(name, desired)?;
            }
            Ok(Some((begun, wanted)))
        })?;
        if let Some((begun, wanted)) = begun {
            if let Some(desired) = wanted {
                events.desired(desired);
            }
            record.state = begun.to();
            self.runner.note_begun(name, begun);
        }
        Ok(())
    }

    /// Records in `record` that the run now runs from this manifest: its file, which a later
    /// operation that has only the record (a sweep's `down`) reopens, and the boot digest of each
    /// VM, which the next update compares.
    pub(crate) fn point_record_at_this_manifest(&self, record: &mut RunRecord) {
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
    ///
    /// Blocking file I/O in a drop, on whichever runtime thread drops the future, on purpose: a
    /// drop can't await, and the record must be settled before the run's lock (taken before this guard, so dropped
    /// after it) lets the next operation in. It is one small local write ([`crate`]'s
    /// exception for the run's state files), atomic (a temporary file renamed over the record),
    /// so a future cut at any `.await` never leaves it half-written.
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
