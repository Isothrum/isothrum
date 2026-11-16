//! What every operation of a run does around its work: its events numbered, masked and
//! logged; the run locked and journaled; what dead operations left settled; the run's state moved
//! when the work begins and when it ends; and exactly one `result` or `error` at the end.

use std::sync::Arc;

use tracing::Instrument;

use super::Running;
use crate::Result;
use crate::clock::rfc3339;
use crate::error::{Error, ErrorCode};
use crate::events::{Emitter, EventKind, Events, ResultData, StoreLog, WarningCode};
use crate::formats::Seen;
use crate::options::RunOptions;
use crate::state::{Desired, Op, Park, RunState, Transition};
use crate::status::MachineState;
use crate::store::{LastOp, OperationId, Outcome, Owner, RunRecord};

/// How long the lookup of a taken port's holder may take: it runs on the failure path only, so a
/// missing answer is better than a slow one.
const HOLDER_LOOKUP: std::time::Duration = std::time::Duration::from_secs(4);

/// What an operation's own work produced.
pub(crate) struct Done<T> {
    /// What the operation returns.
    pub(crate) value: T,
    /// Whether it reached its goal (a check can fail while the operation succeeds).
    pub(crate) ok: bool,
    /// The `result` event's data.
    pub(crate) data: ResultData,
}

impl<T> Done<T> {
    /// An operation that reached its goal, returning `value`, its `result` event carrying `data`.
    pub(crate) fn settled(value: T, data: ResultData) -> Done<T> {
        Done { value, ok: true, data }
    }

    /// An operation whose own report decides whether it reached its goal (an `exec`'s exit):
    /// `ok` is read before the report moves in.
    pub(crate) fn checked(ok: bool, value: T, data: ResultData) -> Done<T> {
        Done { value, ok, data }
    }
}

impl Done<()> {
    /// An operation that moved the run to `state` and returns nothing else: its event says the
    /// state.
    pub(crate) fn moved(state: RunState) -> Done<()> {
        Done::settled((), ResultData::Moved { state })
    }
}

impl Running<'_> {
    /// Runs `work`, the body of `op`, with the run locked, journaled as operation `by` (its third
    /// argument), and its events going to `events`, every secret of `opts` masked.
    pub(super) async fn operate<T>(
        &self,
        op: Op,
        opts: &RunOptions,
        events: &dyn Events,
        work: impl AsyncFnOnce(&RunOptions, &Emitter<'_>, OperationId) -> Result<Done<T>>,
    ) -> Result<T> {
        let runner = self.runner;
        let mask: Vec<String> = opts.secrets.iter().map(|(_, v)| v.expose().to_owned()).collect();
        let emitter = Emitter::new(self.name.as_str(), op, runner.config.clock.as_ref(), events)
            .masking(mask)
            .with_log(StoreLog::new(Arc::clone(runner.store.db())));
        let span = tracing::info_span!("run", run = %self.name, op = %op, operation = tracing::field::Empty);
        let mut cut = CutShort {
            emitter: &emitter,
            armed: true,
        };
        let done = self.locked(op, opts, &emitter, work).instrument(span).await;
        cut.armed = false;
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
        work: impl AsyncFnOnce(&RunOptions, &Emitter<'_>, OperationId) -> Result<Done<T>>,
    ) -> Result<Done<T>> {
        let store = &self.runner.store;
        store.run_dir(&self.name)?;
        let _lock = self.runner.locks.acquire(&self.name, &store.lock_path(&self.name)).await?;
        store.trim_event_log(&self.name)?;
        let id = self.journal_begin(op, opts.operation)?;
        tracing::Span::current().record("operation", id.0);
        emitter.journal_as(id);
        let _flight = self.runner.enter(&self.name, op, id);
        let ended = crate::process::ledger::end_orphans(&self.state_dir().join(crate::process::ledger::DIR)).await;
        if !ended.is_empty() {
            tracing::warn!(?ended, "ended the tools a killed operation on the run left running");
        }
        let before = store.record(&self.name)?.map_or(RunState::Absent, |r| r.state);
        self.begin(op, opts, emitter)?;
        if let Some(during) = op.during() {
            emitter.emit(EventKind::State { from: before, to: during });
        }
        let mut unfinished = Unfinished {
            run: self,
            op,
            before,
            emitter,
            armed: true,
        };
        let done = match work(opts, emitter, id).await {
            Err(taken @ Error::PortInUse { holder: None, .. }) => Err(self.with_holder(taken).await),
            done => done,
        };
        let failure = done.as_ref().err().map(|e| (e.code(), e.changed()));
        let settled = unfinished.settle(failure);
        unfinished.armed = false;
        if let Err(e) = settled {
            if done.is_ok() {
                return Err(e);
            }
            emitter.warning(WarningCode::RecordNotSaved, &format!("the run's record wasn't updated: {e}"));
        }
        done
    }

    /// Records how the run's machines were just parked: what a later reader tells a pause from a
    /// shut down by.
    pub(super) fn note_parked(&self, mode: Park) -> Result<()> {
        self.runner.store.db().write(|tx| {
            let Some(row) = tx.run(&self.name)? else { return Ok(()) };
            let mut record = row.record;
            record.parked = Some(mode);
            tx.save_record(&record, row.desired)
        })
    }

    /// `taken`, a port taken, naming the process that listens on it when one is found within
    /// [`HOLDER_LOOKUP`].
    async fn with_holder(&self, taken: Error) -> Error {
        let Error::PortInUse { port, protocol, .. } = taken else {
            return taken;
        };
        let call = crate::cli::Call {
            runner: self.runner.config.processes.as_ref(),
            cwd: None,
            env: Vec::new(),
            limit: HOLDER_LOOKUP,
            sink: &crate::formats::discard,
            host_env: &self.runner.config.host_env,
        };
        let holder = match crate::encoding::from_wire_name(&protocol) {
            Some(p) => crate::cli::listener::holder(&call, port, p).await,
            None => None,
        };
        Error::PortInUse { port, protocol, holder }
    }

    /// The run's record, or a fresh one for this project.
    fn record_or_fresh(&self) -> Result<RunRecord> {
        let fresh = || RunRecord::fresh(self.name.clone(), self.project.dir.clone(), self.project.format);
        Ok(self.runner.store.record(&self.name)?.unwrap_or_else(fresh))
    }

    /// Saves the run's record and moves the run to the state `op` holds it in, in one transaction.
    fn begin(&self, op: Op, opts: &RunOptions, emitter: &Emitter<'_>) -> Result<()> {
        let recorded = self.runner.store.record(&self.name)?.map(|r| r.state);
        refuse_from(op, recorded, &self.name)?;
        let mut record = self.record_or_fresh()?;
        record.project.clone_from(&self.project.dir);
        record.format = self.project.format;
        if matches!(op, Op::Up | Op::Resume | Op::Down) {
            // Parked no more, however its machines were.
            record.parked = None;
        }
        if op == Op::Up {
            let now = self.runner.config.clock.now();
            record.provider = opts.provider;
            record.docker_context.clone_from(&opts.docker_context);
            record.host_label.clone_from(&opts.host_label);
            record.started_at = Some(rfc3339(now));
            record.expires_at = opts.lifetime.map(|l| crate::clock::unix_seconds(now + l));
            // A start keeps the values it was given, and only those.
            record.env.clone_from(&opts.env);
        } else if op == Op::Update {
            // An update keeps what it changed on top of what the start kept.
            record.env.extend(opts.env.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        let Some(id) = self.runner.operation_of(&self.name) else {
            return self.runner.store.save(&record);
        };
        let name = &self.name;
        let begun = self.runner.store.db().write(|tx| {
            let desired_before = tx.run(name)?.map(|r| r.desired);
            tx.save_record(&record, Desired::Running)?;
            let Some(row) = tx.run(name)? else {
                return Ok(None);
            };
            if op.during().is_none() {
                return Ok(None);
            }
            let begun = Transition::begin(row.state, op, id)?;
            tx.apply(name, begun)?;
            let wanted = Desired::after(op);
            if let Some(desired) = wanted {
                tx.set_desired(name, desired)?;
            }
            Ok(Some((begun, wanted, desired_before.unwrap_or(row.desired))))
        })?;
        if let Some((begun, wanted, desired_before)) = begun {
            if let Some(desired) = wanted {
                emitter.desired(desired);
            }
            self.runner.note_begun(name, begun, desired_before);
        }
        Ok(())
    }

    /// Journals `op` as begun, and ends what dead operations of the run left unfinished.
    fn journal_begin(&self, op: Op, queued: Option<OperationId>) -> Result<OperationId> {
        let name = &self.name;
        let owner = Owner::this_process();
        let now = rfc3339(self.runner.config.clock.now());
        self.runner.store.db().write(|tx| {
            let unfinished: Vec<_> = tx.unfinished(name)?.into_iter().filter(|o| Some(o.id) != queued).collect();
            let (started, waiting): (Vec<_>, Vec<_>) = unfinished.into_iter().partition(|o| o.started_at.is_some());
            let resumed = started.last().map(|o| o.id);
            let row = match queued {
                Some(id) => tx.start_operation(id, name, &owner, resumed)?,
                None => tx.begin_operation(name, op, &owner, resumed)?,
            };
            for left in &started {
                tx.end_operation(left.id, Outcome::Abandoned, Some(ErrorCode::Cancelled))?;
            }
            for orphan in waiting.iter().filter(|o| !o.owner.is_alive()) {
                tx.end_operation(orphan.id, Outcome::Cancelled, Some(ErrorCode::Cancelled))?;
            }
            if let Some(run) = tx.run(name)?
                && let Ok(recovered) = Transition::cut_off(run.state, row.id)
            {
                tx.apply(name, recovered)?;
                let mut record = run.record;
                record.last = Some(LastOp::ended(run.state.moved_by().unwrap_or(op), Some(ErrorCode::Cancelled), now.clone()));
                tx.save_record(&record, run.desired)?;
            }
            Ok(row.id)
        })
    }

    /// Records how `op` ended: the run's move to its goal or to `failed`, or, for a refusal before
    /// anything acted (`failure`'s report unchanged), back where it stood with what was wanted of
    /// it; its last operation; the journal's end. Returns where the run stands.
    fn close(&self, op: Op, failure: Option<(ErrorCode, bool)>) -> Result<Option<RunState>> {
        let name = &self.name;
        let code = failure.map(|(code, _)| code);
        let refused = matches!(failure, Some((_, false)));
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
            let desired = match begun {
                Some((_, before)) if refused => before,
                _ => row.desired,
            };
            tx.save_record(&record, desired)?;
            let moved = begun.map(|(b, _)| match code {
                None => Transition::succeed(b),
                Some(_) if refused => Transition::undone(b),
                Some(code) => Transition::fail(b, code),
            });
            if let Some(t) = moved {
                tx.apply(name, t)?;
            }
            Ok(Some(moved.map_or(row.state, |t| t.to())))
        })
    }
}

/// What the run's recorded state `recorded` (none: never recorded) refuses of `op`, before
/// anything is saved or run:
/// - an `up` of a ready run (`already_running`: use it, or bring it down first);
/// - a `resume` of a run of which nothing is left (`nothing_to_resume`: start it again);
/// - an `update` or a `retire` of a run that isn't ready, and a command, an `attach` or a
///   `provision` in a run of which nothing runs (never started, parked or brought down; a run
///   with a machine down, `failed`, still takes them): `not_running`, start it first. Run
///   anyway, an update would start a parked run's machines, and a command would start the
///   services a one-off depends on.
fn refuse_from(op: Op, recorded: Option<RunState>, name: &crate::RunName) -> Result<()> {
    let run = || name.to_string();
    let ready = recorded == Some(RunState::Ready);
    let started = ready || recorded == Some(RunState::Failed);
    match op {
        Op::Up if ready => Err(Error::AlreadyRunning { run: run() }),
        Op::Resume if matches!(recorded, None | Some(RunState::Absent)) => Err(Error::NothingToResume { run: run() }),
        Op::Update | Op::Retire if !ready => Err(Error::NotRunning { run: run() }),
        Op::Check | Op::Attach | Op::Provision if !started => Err(Error::NotRunning { run: run() }),
        _ => Ok(()),
    }
}

/// An operation whose goal is a ready run (`up`, `resume`, `update`) whose tool ended well, but
/// whose run its tool says isn't whole (a service removed while it started, a machine that died):
/// `not_ready`, naming the machines that aren't, never a ready run with fewer machines.
pub(super) fn not_whole(op: Op, seen: &Seen, state: RunState) -> Result<()> {
    if super::op_goal(op) != Some(RunState::Ready) || !matches!(state, RunState::Failed | RunState::Absent) {
        return Ok(());
    }
    let wrong: Vec<String> = seen
        .machines
        .iter()
        .filter(|m| !matches!(m.state, MachineState::Running | MachineState::Stopped | MachineState::Exited(0)))
        .map(|m| format!("`{}` {}", m.name, m.state))
        .collect();
    Err(Error::NotReady {
        condition: "every machine of the run".to_owned(),
        detail: if wrong.is_empty() {
            format!("the run reads {state}")
        } else {
            wrong.join(", ")
        },
    })
}

/// An operation in flight: dropped before it ended, it ends the event stream with a `cancelled`
/// error, so an operation still ends with exactly one `result` or `error`.
struct CutShort<'e> {
    emitter: &'e Emitter<'e>,
    armed: bool,
}

impl Drop for CutShort<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.emitter.emit(EventKind::Error(crate::ErrorReport::new(
                ErrorCode::Cancelled,
                "the operation was cancelled; the tools it started were stopped",
            )));
        }
    }
}

/// An operation's work on a locked run: dropped before it ended, the run's record says
/// `cancelled` while the run is still locked.
struct Unfinished<'r, 'e> {
    run: &'r Running<'r>,
    op: Op,
    before: RunState,
    emitter: &'e Emitter<'e>,
    armed: bool,
}

impl Unfinished<'_, '_> {
    /// Records how the operation ended (failed with `failure`'s code, having changed the run or
    /// not, else at its goal) and says the state it left the run in.
    fn settle(&self, failure: Option<(ErrorCode, bool)>) -> Result<()> {
        let after = self.run.close(self.op, failure);
        if let Some(during) = self.op.during() {
            let to = after.as_ref().ok().copied().flatten().unwrap_or(self.before);
            self.emitter.emit(EventKind::State { from: during, to });
        }
        after.map(drop)
    }
}

impl Drop for Unfinished<'_, '_> {
    fn drop(&mut self) {
        if self.armed
            && let Err(e) = self.settle(Some((ErrorCode::Cancelled, true)))
        {
            tracing::warn!(error = %e, "the record of a cancelled operation wasn't updated");
        }
    }
}
