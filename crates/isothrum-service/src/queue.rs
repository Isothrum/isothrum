//! The operations: one queue per run, a worker per run with work, so a run's operations run one
//! after the other in the order they were accepted and different runs' run side by side. Each
//! operation's events go to the bus, framed by its `operation` events (queued, started, ended).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use isothrum_client::proto::Command;
use isothrum_client::v1::{Ended, Event, OperationId, OperationPhase, OperationRow, Outcome};
use isothrum_runner::{ErrorCode, EventKind, Events, Op, RunName};
use tokio::sync::{Notify, oneshot};
use tracing::Instrument as _;

use crate::bus::Bus;
use crate::engine::{Engine, Refusal};

/// How many ended operations are kept for `operations`.
const ROWS_KEPT: usize = 1000;

/// The service's operations.
pub(crate) struct Queue {
    engine: Arc<dyn Engine>,
    bus: Arc<Bus>,
    state: Mutex<State>,
    changed: Notify,
}

/// What the queue holds.
struct State {
    accepting: bool,
    next_id: i64,
    /// The operations waiting, by run, oldest first.
    pending: HashMap<RunName, VecDeque<Job>>,
    /// The operation running on each run that has one, and how to cut it short.
    running: HashMap<RunName, Current>,
    /// The runs whose worker is there (running an operation, or about to take the next).
    workers: std::collections::HashSet<RunName>,
    rows: BTreeMap<OperationId, OperationRow>,
}

/// An accepted operation.
struct Job {
    id: OperationId,
    command: Command,
}

/// The operation running on a run.
struct Current {
    id: OperationId,
    cancel: Option<oneshot::Sender<()>>,
}

impl Queue {
    /// A queue running its operations on `engine`, their events to `bus`, its first operation's
    /// id `first`.
    pub(crate) fn new(engine: Arc<dyn Engine>, bus: Arc<Bus>, first: i64) -> Queue {
        Queue {
            engine,
            bus,
            state: Mutex::new(State {
                accepting: true,
                next_id: first.max(1),
                pending: HashMap::new(),
                running: HashMap::new(),
                workers: std::collections::HashSet::new(),
                rows: BTreeMap::new(),
            }),
            changed: Notify::new(),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Accepts the operation `command`: its id, its run, and how many of the run's operations
    /// are ahead of it.
    ///
    /// # Errors
    ///
    /// The run can't be opened, or the service is stopping.
    pub(crate) fn submit(self: &Arc<Self>, command: Command) -> Result<(OperationId, RunName, u32), Refusal> {
        let (Some(op), Some((target, _))) = (command.op(), command.target()) else {
            return Err(Refusal::new(ErrorCode::Usage, "not an operation"));
        };
        let run = self.engine.resolve(target)?;
        let mut state = self.state();
        if !state.accepting {
            return Err(Refusal::new(ErrorCode::Cancelled, "the service is stopping: it takes no new operation"));
        }
        let id = OperationId(state.next_id);
        state.next_id += 1;
        let ahead = state.pending.get(&run).map_or(0, VecDeque::len) + usize::from(state.running.contains_key(&run));
        let position = u32::try_from(ahead).unwrap_or(u32::MAX);
        state.pending.entry(run.clone()).or_default().push_back(Job { id, command });
        state.rows.insert(id, OperationRow::new(id, run.clone(), op));
        trim(&mut state.rows);
        self.bus.publish(Event::phase(&run, op, id, OperationPhase::Queued, Some(position)));
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
        let op = job.command.op().unwrap_or(Op::Up);
        self.bus.publish(Event::phase(run, op, job.id, OperationPhase::Started, None));
        tracing::info!(%run, %op, operation = %job.id, "operation started");
        let sink = Sink {
            bus: &self.bus,
            operation: job.id,
            outcome_seen: AtomicBool::new(false),
        };
        // Every line the engine and its tools log during the operation carries its id.
        let span = tracing::info_span!("operation", operation = %job.id, %run, %op);
        let ok = tokio::select! {
            ok = self.engine.execute(run, &job.command, &sink).instrument(span) => Some(ok),
            _ = cancelled => None,
        };
        if ok.is_none() && !sink.outcome_seen.load(Ordering::SeqCst) {
            self.bus.publish(cancelled_event(run, op, job.id));
        }
        let outcome = match ok {
            Some(true) => Outcome::Succeeded,
            Some(false) => Outcome::Failed,
            None => Outcome::Cancelled,
        };
        tracing::info!(%run, %op, operation = %job.id, ?outcome, "operation ended");
        let mut state = self.state();
        state.running.remove(run);
        end_row(&mut state, job.id, outcome);
        self.bus.publish(Event::phase(run, op, job.id, OperationPhase::Ended, None));
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
            self.drop_queued(&mut state, &run, &job);
            return true;
        }
        let Some(current) = state.running.values_mut().find(|c| c.id == id) else {
            return false;
        };
        if let Some(cancel) = current.cancel.take()
            && cancel.send(()).is_err()
        {
            tracing::debug!(operation = %id, "the operation was ending: nothing left to cancel");
        }
        if let Some(row) = state.rows.get_mut(&id) {
            row.cancel_requested = true;
        }
        true
    }

    /// Ends the queued `job` of `run` without running it.
    fn drop_queued(&self, state: &mut State, run: &RunName, job: &Job) {
        let op = job.command.op().unwrap_or(Op::Up);
        if let Some(row) = state.rows.get_mut(&job.id) {
            row.cancel_requested = true;
        }
        end_row(state, job.id, Outcome::Cancelled);
        self.bus.publish(cancelled_event(run, op, job.id));
        self.bus.publish(Event::phase(run, op, job.id, OperationPhase::Ended, None));
    }

    /// Run `run`'s latest operations, newest first.
    pub(crate) fn operations(&self, run: &RunName, limit: usize) -> Vec<OperationRow> {
        self.state().rows.values().rev().filter(|r| &r.run == run).take(limit).cloned().collect()
    }

    /// Whether an operation is queued or running.
    pub(crate) fn busy(&self) -> bool {
        !self.state().workers.is_empty()
    }

    /// Takes no new operation, drops the queued ones and cuts the running ones short.
    pub(crate) fn close(&self) {
        let mut state = self.state();
        state.accepting = false;
        let queued: Vec<(RunName, Job)> = state
            .pending
            .iter_mut()
            .flat_map(|(run, jobs)| jobs.drain(..).map(|job| (run.clone(), job)).collect::<Vec<_>>())
            .collect();
        for (run, job) in &queued {
            self.drop_queued(&mut state, run, job);
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

/// Marks operation `id` ended with `outcome`.
fn end_row(state: &mut State, id: OperationId, outcome: Outcome) {
    if let Some(row) = state.rows.get_mut(&id) {
        let code = match outcome {
            Outcome::Cancelled | Outcome::Abandoned => Some(ErrorCode::Cancelled),
            Outcome::Succeeded | Outcome::Failed => None,
        };
        row.ended = Some(Ended {
            at: isothrum_runner::rfc3339(std::time::SystemTime::now()),
            outcome,
            code,
        });
    }
}

/// Keeps the newest rows.
fn trim(rows: &mut BTreeMap<OperationId, OperationRow>) {
    while rows.len() > ROWS_KEPT {
        rows.pop_first();
    }
}

/// The `error` of an operation cancelled before the engine said how it ended.
fn cancelled_event(run: &RunName, op: Op, id: OperationId) -> Event {
    let kind = EventKind::Error {
        code: ErrorCode::Cancelled,
        message: "the operation was cancelled".to_owned(),
        step: None,
        timeout: None,
    };
    Event::of_runner(&isothrum_runner::Event::new(0, Some(run.to_string()), Some(op), kind), Some(id))
}

/// Where an operation's events go: the bus, with the operation's id.
struct Sink<'a> {
    bus: &'a Bus,
    operation: OperationId,
    outcome_seen: AtomicBool,
}

impl Events for Sink<'_> {
    fn emit(&self, event: &isothrum_runner::Event) {
        let event = Event::of_runner(event, Some(self.operation));
        if event.is_outcome() {
            self.outcome_seen.store(true, Ordering::SeqCst);
        }
        self.bus.publish(event);
    }
}
