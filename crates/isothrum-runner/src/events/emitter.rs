//! One operation's events: numbered, timed, masked, kept in the run's own log, and handed to the
//! embedder's sink.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use super::WarningCode;
use super::{EVENTS_VERSION, Event, EventKind, Events, Stream};
use crate::clock::{Clock, rfc3339};
use crate::error::Error;
use crate::state::Op;
use crate::store::{OperationId, Store};

/// Where an operation's events are kept: the home's store, under the operation once it is
/// journaled.
pub(crate) struct StoreLog {
    store: Arc<Store>,
    operation: Option<OperationId>,
}

impl StoreLog {
    /// Events kept in `store`, under no operation yet.
    pub(crate) fn new(store: Arc<Store>) -> StoreLog {
        StoreLog { store, operation: None }
    }

    /// Appends `event`, and journals it when it is a step's.
    fn append(&self, event: &Event) -> crate::Result<()> {
        self.store.write(|tx| {
            tx.append_event(self.operation, event)?;
            if let (Some(id), EventKind::Step { step, state, .. }) = (self.operation, &event.kind) {
                tx.step(id, step, *state)?;
            }
            Ok(())
        })
    }
}

/// What a masked value reads as.
pub(crate) const MASK: &str = "***";

/// The id a read-only query's calls are reported under.
const QUERY: &str = "query";

/// The events of one operation on one run.
pub(crate) struct Emitter<'a> {
    run: String,
    /// The operation, none for a read-only query's emitter, which says nothing.
    op: Option<Op>,
    seq: AtomicU64,
    clock: &'a dyn Clock,
    sink: &'a dyn Events,
    log: Mutex<Option<StoreLog>>,
    /// The values never shown, longest first (so a value holding another is masked whole).
    secrets: Mutex<Vec<String>>,
}

impl<'a> Emitter<'a> {
    /// Events of `op` on `run`, timed by `clock`, to `sink`.
    pub(crate) fn new(run: &str, op: Op, clock: &'a dyn Clock, sink: &'a dyn Events) -> Emitter<'a> {
        Emitter::of(run, Some(op), clock, sink)
    }

    /// The emitter of a read-only query of `run` (`status`, a console's command): no operation,
    /// and it says nothing.
    pub(crate) fn silent(run: &str, clock: &'a dyn Clock) -> Emitter<'a> {
        Emitter::of(run, None, clock, &|_: &crate::events::Event| {})
    }

    fn of(run: &str, op: Option<Op>, clock: &'a dyn Clock, sink: &'a dyn Events) -> Emitter<'a> {
        Emitter {
            run: run.to_owned(),
            op,
            seq: AtomicU64::new(0),
            clock,
            sink,
            log: Mutex::new(None),
            secrets: Mutex::new(Vec::new()),
        }
    }

    /// The name the operation's own calls are reported under: the operation's (`retire`), or
    /// `query` for a read-only query's.
    pub(crate) fn operation_name(&self) -> &'static str {
        self.op.map_or(QUERY, Op::as_str)
    }

    /// Also appends each event to the home's store (its event table), and journals each step
    /// under `log`'s operation once it has one.
    pub(crate) fn with_log(self, log: StoreLog) -> Emitter<'a> {
        *self.log.lock().unwrap_or_else(PoisonError::into_inner) = Some(log);
        self
    }

    /// From now on, the events belong to journaled operation `id`, and its steps are journaled.
    pub(crate) fn journal_as(&self, id: OperationId) {
        if let Some(log) = self.log.lock().unwrap_or_else(PoisonError::into_inner).as_mut() {
            log.operation = Some(id);
        }
    }

    /// Masks `values` wherever they would appear.
    pub(crate) fn masking(self, values: impl IntoIterator<Item = String>) -> Emitter<'a> {
        self.hide(values);
        self
    }

    /// Masks `values` too, from the next event on (a sensitive output once it is read).
    pub(crate) fn hide(&self, values: impl IntoIterator<Item = String>) {
        let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        secrets.extend(values.into_iter().filter(|v| !v.is_empty()));
        secrets.sort_by_key(|v| std::cmp::Reverse(v.len()));
        secrets.dedup();
    }

    /// `text` with every secret value replaced by `***`.
    pub(crate) fn mask(&self, text: &str) -> String {
        let secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        secrets.iter().fold(text.to_owned(), |t, s| t.replace(s.as_str(), MASK))
    }

    /// Says what the user now wants of the run.
    pub(crate) fn desired(&self, desired: crate::state::Desired) {
        self.emit(EventKind::Desired { desired });
    }

    /// Says where `row`, a resource of the run, stands now.
    pub(crate) fn resource(&self, row: &crate::store::ResourceRow) {
        self.emit(EventKind::Resource {
            key: row.key.clone(),
            kind: row.kind,
            state: row.state,
            native_id: row.native_id.clone(),
        });
    }

    /// Emits `kind`, masked.
    pub(crate) fn emit(&self, kind: EventKind) {
        let kind = self.masked(kind);
        // The sequence number is taken under the log's lock: taken before it, two emits at once
        // could write N+1 before N to the log (Clean Code appendix A).
        let log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let event = Event {
            v: EVENTS_VERSION,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            time: rfc3339(self.clock.now()),
            run: Some(self.run.clone()),
            op: self.op,
            operation: log.as_ref().and_then(|l| l.operation),
            kind,
        };
        if let Some(log) = log.as_ref() {
            // The event and the journal's step are one transaction. The log is what a UI that
            // reattaches replays: losing an event never fails the run.
            if let Err(e) = log.append(&event) {
                tracing::warn!(error = %e, "an event wasn't logged");
            }
        }
        drop(log);
        self.sink.emit(&event);
    }

    fn masked(&self, kind: EventKind) -> EventKind {
        match kind {
            EventKind::Log { step, stream, line } => EventKind::Log {
                step,
                stream,
                line: self.mask(&line),
            },
            EventKind::Warning { code, message } => EventKind::Warning {
                code,
                message: self.mask(&message),
            },
            EventKind::Error { code, message, step, timeout } => EventKind::Error {
                code,
                message: self.mask(&message),
                step,
                timeout: timeout.map(|t| t.masked(|l| self.mask(l))),
            },
            EventKind::Resource { key, kind, state, native_id } => EventKind::Resource {
                key: self.mask(&key),
                kind,
                state,
                native_id: native_id.map(|id| self.mask(&id)),
            },
            EventKind::Result { ok, data } => EventKind::Result {
                ok,
                data: data.masked(|s| self.mask(s)),
            },
            other => other,
        }
    }

    /// A line a step's tool printed.
    pub(crate) fn log(&self, step: &str, stream: Stream, line: &str) {
        self.emit(EventKind::Log {
            step: step.to_owned(),
            stream,
            line: line.to_owned(),
        });
    }

    /// A warning.
    pub(crate) fn warning(&self, code: WarningCode, message: &str) {
        self.emit(EventKind::Warning {
            code,
            message: message.to_owned(),
        });
    }

    /// The operation's failure.
    pub(crate) fn error(&self, err: &Error) {
        self.emit(EventKind::Error {
            code: err.code(),
            message: err.to_string(),
            step: err.step().map(str::to_owned),
            timeout: err.timed_out().cloned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::clock::FixedClock;
    use crate::events::Recorder;

    #[test]
    fn events_are_numbered_timed_and_masked() {
        let clock = FixedClock(UNIX_EPOCH + Duration::from_secs(1_792_065_600));
        let sink = Recorder::new();
        let e = Emitter::new("web-portal", Op::Up, &clock, &sink).masking(["token-x".to_owned(), "token-x-long".to_owned(), String::new()]);
        e.log("up", Stream::Stderr, "seeding token-x-long and token-x");
        e.emit(EventKind::Result {
            ok: true,
            data: crate::ResultData::Started {
                state: crate::RunState::Ready,
                outputs: vec![crate::ShownOutput::new("url", Some("at token-x".into()))],
            },
        });
        let got = sink.events();
        assert_eq!(got.iter().map(|event| event.seq).collect::<Vec<_>>(), [1, 2]);
        assert!(
            got.iter()
                .all(|event| event.time == "2026-10-15T12:00:00Z" && event.run.as_deref() == Some("web-portal") && event.op == Some(Op::Up))
        );
        assert!(
            matches!(&got[0].kind, EventKind::Log { line, .. } if line == "seeding *** and ***"),
            "{:?}",
            got[0]
        );
        assert!(matches!(&got[1].kind, EventKind::Result { data: crate::ResultData::Started { outputs, .. }, .. } if outputs[0].value.as_deref() == Some("at ***")));
    }

    #[test]
    fn the_store_keeps_each_event_and_journals_each_step() {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(home.path()).unwrap());
        let run = crate::name::RunName::new("r").unwrap();
        let op = store
            .write(|tx| tx.begin_operation(&run, Op::Down, &crate::store::Owner::this_process(), None))
            .unwrap();
        let clock = FixedClock(UNIX_EPOCH);
        let sink = Recorder::new();
        let e = Emitter::new("r", Op::Down, &clock, &sink).with_log(StoreLog::new(Arc::clone(&store)));
        e.journal_as(op.id);
        e.emit(EventKind::Step {
            step: "down".into(),
            state: crate::events::StepState::Done,
            note: None,
        });
        e.warning(WarningCode::StepTolerated, "a tolerated step failed");
        drop(e);
        let (logged, unreadable) = store.read(|tx| tx.events_after(crate::store::EventCursor::default(), Some(&run), 10)).unwrap();
        assert_eq!(unreadable, 0);
        assert_eq!(logged.into_iter().map(|(_, event)| event).collect::<Vec<_>>(), sink.events());
        assert_eq!(store.read(|tx| tx.steps(op.id)).unwrap(), [("down".to_owned(), crate::events::StepState::Done)]);
    }

    #[test]
    fn events_emitted_from_several_threads_at_once_reach_the_log_in_their_order() {
        // More threads than an operation ever has, each emitting at once, again and again: the
        // store's numbers rise event by event, none missing, none twice.
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(home.path()).unwrap());
        let run = crate::name::RunName::new("r").unwrap();
        let op = store
            .write(|tx| tx.begin_operation(&run, Op::Up, &crate::store::Owner::this_process(), None))
            .unwrap();
        let clock = FixedClock(UNIX_EPOCH);
        let sink = Recorder::new();
        let e = Emitter::new("r", Op::Up, &clock, &sink).with_log(StoreLog::new(Arc::clone(&store)));
        e.journal_as(op.id);
        let (threads, each) = (8, 250);
        let start = std::sync::Barrier::new(threads);
        std::thread::scope(|s| {
            for t in 0..threads {
                let (e, start) = (&e, &start);
                s.spawn(move || {
                    start.wait();
                    for i in 0..each {
                        e.log("up", Stream::Stdout, &format!("thread {t} line {i}"));
                    }
                });
            }
        });
        drop(e);
        let total = threads * each;
        let (logged, unreadable) = store
            .read(|tx| tx.events_after(crate::store::EventCursor::default(), Some(&run), total + 1))
            .unwrap();
        assert_eq!(unreadable, 0);
        let seqs: Vec<u64> = logged.into_iter().map(|(_, event)| event.seq).collect();
        let expected: Vec<u64> = (1..=u64::try_from(total).unwrap()).collect();
        assert_eq!(seqs, expected);
    }
}
