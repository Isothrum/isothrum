//! One operation's events: numbered, timed, masked, kept in the run's own log, and handed to the
//! embedder's sink.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use super::WarningCode;
use super::{EVENTS_VERSION, Event, EventKind, Events, ReadyState, StepState, Stream};
use crate::clock::{Clock, rfc3339};
use crate::error::Error;
use crate::state::Op;

/// What a masked value reads as.
pub(crate) const MASK: &str = "***";

/// The events of one operation on one run.
pub(crate) struct Emitter<'a> {
    run: String,
    op: Op,
    seq: AtomicU64,
    clock: &'a dyn Clock,
    sink: &'a dyn Events,
    log: Option<Mutex<File>>,
    /// The values never shown, longest first (so a value holding another is masked whole).
    secrets: Mutex<Vec<String>>,
}

impl<'a> Emitter<'a> {
    /// Events of `op` on `run`, timed by `clock`, to `sink`.
    pub(crate) fn new(run: &str, op: Op, clock: &'a dyn Clock, sink: &'a dyn Events) -> Emitter<'a> {
        Emitter {
            run: run.to_owned(),
            op,
            seq: AtomicU64::new(0),
            clock,
            sink,
            log: None,
            secrets: Mutex::new(Vec::new()),
        }
    }

    /// The operation the events are of.
    pub(crate) fn op(&self) -> Op {
        self.op
    }

    /// Also appends each event to `log` (the run's `events.jsonl`).
    pub(crate) fn with_log(mut self, log: File) -> Emitter<'a> {
        self.log = Some(Mutex::new(log));
        self
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

    /// Emits `kind`, masked.
    pub(crate) fn emit(&self, kind: EventKind) {
        let event = Event {
            v: EVENTS_VERSION,
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            time: rfc3339(self.clock.now()),
            run: Some(self.run.clone()),
            op: Some(self.op),
            kind: self.masked(kind),
        };
        if let Some(log) = &self.log {
            let written = serde_json::to_string(&event)
                .map_err(std::io::Error::from)
                .and_then(|text| writeln!(log.lock().unwrap_or_else(PoisonError::into_inner), "{text}"));
            // The log is a convenience for a UI that reattaches: losing a line never fails the run.
            if let Err(e) = written {
                tracing::warn!(error = %e, "an event wasn't logged");
            }
        }
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
            EventKind::Ready { condition, state, detail } => EventKind::Ready {
                condition,
                state,
                detail: self.mask(&detail),
            },
            EventKind::Check {
                runner,
                from,
                name,
                ok,
                detail,
            } => EventKind::Check {
                runner,
                from,
                name,
                ok,
                detail: self.mask(&detail),
            },
            EventKind::Result { ok, data } => EventKind::Result {
                ok,
                data: self.mask_json(data),
            },
            other => other,
        }
    }

    fn mask_json(&self, data: serde_json::Value) -> serde_json::Value {
        match data {
            serde_json::Value::String(s) => serde_json::Value::String(self.mask(&s)),
            serde_json::Value::Array(items) => items.into_iter().map(|v| self.mask_json(v)).collect(),
            serde_json::Value::Object(map) => map.into_iter().map(|(k, v)| (k, self.mask_json(v))).collect(),
            other => other,
        }
    }

    /// A step's state.
    pub(crate) fn step(&self, step: &str, state: StepState, note: Option<&str>) {
        self.emit(EventKind::Step {
            step: step.to_owned(),
            state,
            note: note.map(str::to_owned),
        });
    }

    /// Runs `work` as step `step`: its `running` event (with `note`) first, then `done` or
    /// `failed` as it ended.
    pub(crate) async fn stepped<T>(&self, step: &str, note: Option<&str>, work: impl Future<Output = crate::Result<T>>) -> crate::Result<T> {
        self.step(step, StepState::Running, note);
        let done = work.await;
        self.step(step, if done.is_ok() { StepState::Done } else { StepState::Failed }, None);
        done
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

    /// A ready condition's state.
    pub(crate) fn ready(&self, condition: &str, state: ReadyState, detail: &str) {
        self.emit(EventKind::Ready {
            condition: condition.to_owned(),
            state,
            detail: detail.to_owned(),
        });
    }

    /// The operation's failure.
    pub(crate) fn error(&self, err: &Error) {
        let step = match err {
            Error::ToolMissing { step, .. } | Error::ToolFailed { step, .. } | Error::Timeout { step, .. } => Some(step.clone()),
            _ => None,
        };
        let timeout = match err {
            Error::Timeout { timed_out, .. } => Some(timed_out.clone()),
            _ => None,
        };
        self.emit(EventKind::Error {
            code: err.code(),
            message: err.to_string(),
            step,
            timeout,
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
            data: serde_json::json!({"message": ["at token-x"]}),
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
        assert!(matches!(&got[1].kind, EventKind::Result { data, .. } if data["message"][0] == "at ***"));
    }

    #[test]
    fn the_runs_log_keeps_each_event_as_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let clock = FixedClock(UNIX_EPOCH);
        let sink = Recorder::new();
        let e = Emitter::new("r", Op::Down, &clock, &sink).with_log(File::create(&path).unwrap());
        e.step("down", StepState::Done, None);
        e.warning(WarningCode::StepTolerated, "a tolerated step failed");
        drop(e);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Event> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines, sink.events());
    }
}
