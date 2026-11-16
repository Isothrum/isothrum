//! The operations: one queue per run, a worker per run with work, so a run's operations run one
//! after the other in the order they were accepted and different runs' run side by side. Each
//! operation is journaled as queued when it is accepted (its id is the one the client and every
//! event see), and framed by its `operation` events (queued, started, ended), kept in the store.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use isothrum_client::proto::Command;
use isothrum_client::v1::{Event, OperationPhase, is_outcome, phase_event};
use isothrum_runner::store::{OperationId, OperationRow, Outcome, Owner, Store};
use isothrum_runner::{ErrorCode, EventKind, Events, Op, RunName};
use tokio::sync::{Notify, oneshot};
use tracing::Instrument as _;

use crate::engine::{Engine, Journal, Refusal};
use crate::feed::Feed;

/// The service's operations.
pub(crate) struct Queue {
    engine: Arc<dyn Engine>,
    store: Arc<Store>,
    feed: Arc<Feed>,
    owner: Owner,
    state: Mutex<State>,
    changed: Notify,
}

/// What the queue holds.
#[derive(Default)]
struct State {
    stopping: bool,
    /// The operations waiting, by run, oldest first.
    pending: HashMap<RunName, VecDeque<Job>>,
    /// The operation running on each run that has one, and how to cut it short.
    running: HashMap<RunName, Current>,
    /// The runs whose worker is there (running an operation, or about to take the next).
    workers: HashSet<RunName>,
}

/// An accepted operation.
struct Job {
    id: OperationId,
    op: Op,
    command: Command,
}

/// The operation running on a run.
struct Current {
    id: OperationId,
    cancel: Option<oneshot::Sender<()>>,
}

impl Queue {
    /// A queue running its operations on `engine`, journaled in `store` as `owner`'s, their
    /// events to `feed`.
    pub(crate) fn new(engine: Arc<dyn Engine>, store: Arc<Store>, feed: Arc<Feed>, owner: Owner) -> Queue {
        Queue {
            engine,
            store,
            feed,
            owner,
            state: Mutex::default(),
            changed: Notify::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keeps the service's own event of operation `id`; a store that fails is logged (the
    /// operation goes on: its events are what the runner keeps).
    fn keep(&self, id: OperationId, event: &Event) {
        if let Err(refusal) = self.feed.keep(Some(id), event) {
            tracing::warn!(operation = %id, error = %refusal.0.message, "an operation's event couldn't be kept");
        }
    }

    /// Accepts the operation `command`: its id (journaled as queued), its run, and how many of
    /// the run's operations are ahead of it.
    ///
    /// # Errors
    ///
    /// The run can't be opened, the store can't be written, or the service is stopping.
    pub(crate) fn submit(self: &Arc<Self>, command: Command) -> Result<(OperationId, RunName, u32), Refusal> {
        let (Some(op), Some((target, _))) = (command.op(), command.target()) else {
            return Err(Refusal::new(ErrorCode::Usage, "not an operation"));
        };
        let run = self.engine.resolve(target)?;
        let mut state = self.state();
        if state.stopping {
            return Err(Refusal::new(ErrorCode::Cancelled, "the service is stopping: it takes no new operation"));
        }
        let id = self.store.write(|tx| tx.queue_operation(&run, op, &self.owner))?.id;
        let ahead = state.pending.get(&run).map_or(0, VecDeque::len) + usize::from(state.running.contains_key(&run));
        let position = u32::try_from(ahead).unwrap_or(u32::MAX);
        state.pending.entry(run.clone()).or_default().push_back(Job { id, op, command });
        self.keep(id, &phase_event(&run, (op, id), OperationPhase::Queued, Some(position), None));
        if state.workers.insert(run.clone()) {
            let queue = Arc::clone(self);
            let worker_run = run.clone();
            tokio::spawn(async move { queue.work(worker_run).await });
        }
        drop(state);
        self.changed.notify_waiters();
        Ok((id, run, position))
    }

    /// Runs `run`'s operations, oldest first, until none waits.
    async fn work(self: Arc<Self>, run: RunName) {
        loop {
            let (job, cancelled) = {
                let mut state = self.state();
                let Some(job) = state.pending.get_mut(&run).and_then(VecDeque::pop_front) else {
                    state.pending.remove(&run);
                    state.workers.remove(&run);
                    drop(state);
                    self.changed.notify_waiters();
                    return;
                };
                let (cancel, cancelled) = oneshot::channel();
                state.running.insert(
                    run.clone(),
                    Current {
                        id: job.id,
                        cancel: Some(cancel),
                    },
                );
                (job, cancelled)
            };
            self.run_one(&run, &job, cancelled).await;
        }
    }

    /// Runs `job` on `run` to its end, or until `cancelled`.
    async fn run_one(&self, run: &RunName, job: &Job, cancelled: oneshot::Receiver<()>) {
        let (id, op) = (job.id, job.op);
        self.keep(id, &phase_event(run, (op, id), OperationPhase::Started, None, None));
        let sink = Sink {
            feed: &self.feed,
            operation: id,
            outcome_seen: AtomicBool::new(false),
        };
        // Every line the engine and its tools log during the operation carries its id.
        let span = tracing::info_span!("operation", operation = %id, %run, %op);
        let ok = tokio::select! {
            ok = self.engine.execute(run, &job.command, id, &sink).instrument(span) => Some(ok),
            _ = cancelled => None,
        };
        if !sink.outcome_seen.load(Ordering::SeqCst) {
            // Cut short before the engine said how it ended.
            sink.keep(error_event(run, op, ErrorCode::Cancelled, "the operation was cancelled"));
        }
        let outcome = self.close(id, ok);
        tracing::info!(%run, %op, operation = %id, ?outcome, "operation ended");
        let mut state = self.state();
        state.running.remove(run);
        self.keep(id, &phase_event(run, (op, id), OperationPhase::Ended, None, Some(outcome)));
    }

    /// How operation `id` ended, as the journal says; ended here when the engine left it open
    /// (refused before it began, or cut short before it could say).
    fn close(&self, id: OperationId, ok: Option<bool>) -> Outcome {
        let closed = self.store.write(|tx| {
            let Some(row) = tx.operation(id)? else {
                return Ok(Outcome::Cancelled);
            };
            if let Some(ended) = row.ended {
                return Ok(ended.outcome);
            }
            // A queued operation that never began can only end cancelled.
            let outcome = match ok {
                Some(true) if row.started_at.is_some() => Outcome::Succeeded,
                Some(false) if row.started_at.is_some() => Outcome::Failed,
                _ => Outcome::Cancelled,
            };
            tx.end_operation(id, outcome, (outcome == Outcome::Cancelled).then_some(ErrorCode::Cancelled))?;
            Ok(outcome)
        });
        closed.unwrap_or_else(|e| {
            tracing::warn!(operation = %id, error = %e, "the operation's end couldn't be journaled");
            if ok == Some(true) { Outcome::Succeeded } else { Outcome::Failed }
        })
    }

    /// Cancels `id`: a queued operation is dropped, a running one cut short. False when it isn't
    /// queued or running.
    pub(crate) fn cancel(&self, id: OperationId) -> bool {
        let mut state = self.state();
        let queued = state.pending.iter_mut().find_map(|(run, jobs)| {
            let at = jobs.iter().position(|j| j.id == id)?;
            Some((run.clone(), jobs.remove(at)?))
        });
        if let Some((run, job)) = queued {
            self.drop_queued(&run, &job);
            return true;
        }
        let Some(current) = state.running.values_mut().find(|c| c.id == id) else {
            return false;
        };
        if let Err(e) = self.store.write(|tx| tx.request_cancel(id)) {
            tracing::warn!(operation = %id, error = %e, "the cancel couldn't be journaled");
        }
        if let Some(cancel) = current.cancel.take()
            && cancel.send(()).is_err()
        {
            tracing::debug!(operation = %id, "the operation was ending: nothing left to cancel");
        }
        true
    }

    /// Ends the queued `job` of `run` without running it.
    fn drop_queued(&self, run: &RunName, job: &Job) {
        let (id, op) = (job.id, job.op);
        let journaled = self.store.write(|tx| {
            tx.request_cancel(id)?;
            tx.end_operation(id, Outcome::Cancelled, Some(ErrorCode::Cancelled))
        });
        if let Err(e) = journaled {
            tracing::warn!(operation = %id, error = %e, "the cancel couldn't be journaled");
        }
        let mut cancelled = error_event(run, op, ErrorCode::Cancelled, "the operation was cancelled before it began");
        cancelled.operation = Some(id);
        self.keep(id, &cancelled);
        self.keep(id, &phase_event(run, (op, id), OperationPhase::Ended, None, Some(Outcome::Cancelled)));
    }

    /// Run `run`'s latest operations, newest first.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub(crate) fn operations(&self, run: &RunName, limit: usize) -> Result<Vec<OperationRow>, Refusal> {
        Ok(self.store.read(|tx| tx.operations(run, limit))?)
    }

    /// Whether an operation of `run` is queued or running.
    pub(crate) fn has_work(&self, run: &RunName) -> bool {
        self.state().workers.contains(run)
    }

    /// Whether an operation is queued or running.
    pub(crate) fn busy(&self) -> bool {
        !self.state().workers.is_empty()
    }

    /// Takes no new operation when none is queued or running; else the ones that are (oldest
    /// first), and nothing changes. One look at the queue: an operation can't slip in between.
    pub(crate) fn stop_if_idle(&self) -> Result<(), Vec<OperationId>> {
        let mut state = self.state();
        let mut operations: Vec<OperationId> = state
            .running
            .values()
            .map(|c| c.id)
            .chain(state.pending.values().flatten().map(|j| j.id))
            .collect();
        if operations.is_empty() && state.workers.is_empty() {
            state.stopping = true;
            return Ok(());
        }
        operations.sort_unstable();
        Err(operations)
    }

    /// Takes no new operation, drops the queued ones and cuts the running ones short.
    pub(crate) fn close_all(&self) {
        let mut state = self.state();
        state.stopping = true;
        let queued: Vec<(RunName, Job)> = state
            .pending
            .iter_mut()
            .flat_map(|(run, jobs)| jobs.drain(..).map(|job| (run.clone(), job)).collect::<Vec<_>>())
            .collect();
        for (run, job) in &queued {
            self.drop_queued(run, job);
        }
        for current in state.running.values_mut() {
            if let Some(cancel) = current.cancel.take()
                && cancel.send(()).is_err()
            {
                tracing::debug!(operation = %current.id, "the operation was ending: nothing left to cancel");
            }
        }
    }

    /// Returns once no operation is queued or running.
    pub(crate) async fn drained(&self) {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.busy() {
                return;
            }
            changed.await;
        }
    }
}

/// An `error` event of `op` on `run`.
fn error_event(run: &RunName, op: Op, code: ErrorCode, message: &str) -> Event {
    let kind = EventKind::Error(isothrum_runner::ErrorReport::new(code, message));
    Event::new(0, Some(run.to_string()), Some(op), kind)
}

/// Where an operation's events go: the runner's are kept by the runner, and only wake the
/// subscribers; the engine's own are kept here.
struct Sink<'a> {
    feed: &'a Feed,
    operation: OperationId,
    outcome_seen: AtomicBool,
}

impl Events for Sink<'_> {
    fn emit(&self, event: &Event) {
        if is_outcome(event) {
            self.outcome_seen.store(true, Ordering::SeqCst);
        }
        self.feed.wake();
    }
}

impl Journal for Sink<'_> {
    fn keep(&self, mut event: Event) {
        if is_outcome(&event) {
            self.outcome_seen.store(true, Ordering::SeqCst);
        }
        event.operation = Some(self.operation);
        if let Err(refusal) = self.feed.keep(Some(self.operation), &event) {
            tracing::warn!(operation = %self.operation, error = %refusal.0.message, "an operation's event couldn't be kept");
        }
    }
}
