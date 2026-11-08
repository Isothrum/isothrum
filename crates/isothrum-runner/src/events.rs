//! The event stream: what an operation does, as JSON values an embedder shows or a command line
//! prints one per line (`isothrum … --json`). It evolves from Isoloom's lifecycle events (the
//! `event` tag, `step`/`log`/`check`/`result`), adding the run, the operation, a sequence number,
//! the time on every event, and its own version `v`, independent of the manifest's.
//!
//! Rules: a secret's value never appears (masked before the event is built); `log` lines are
//! decoded lossily; an operation ends with exactly one `result` or `error`.

mod emitter;

use std::io::Write;
use std::sync::{Mutex, PoisonError};

use isoloom_manifest::OutputRole;
use serde::{Deserialize, Serialize};

pub(crate) use emitter::Emitter;

use crate::error::ErrorCode;
use crate::state::{Op, RunState};

/// The event stream's version.
pub const EVENTS_VERSION: u32 = 0;

/// Where a step stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StepState {
    /// Started.
    Running,
    /// Finished and succeeded.
    Done,
    /// Finished and failed (an `error` follows unless the step's failure is tolerated).
    Failed,
    /// Not run: an earlier step failed.
    Skipped,
}

/// Which output of a tool a line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stream {
    /// Its standard output.
    Stdout,
    /// Its standard error.
    Stderr,
}

/// Where a ready condition stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReadyState {
    /// Not yet; polled again.
    Waiting,
    /// It holds.
    Ok,
    /// It didn't hold in time.
    Failed,
}

/// What happened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// The run moved from one state to another.
    State {
        /// Before.
        from: RunState,
        /// After.
        to: RunState,
    },
    /// A step started, finished, failed or was skipped.
    Step {
        /// The step's id.
        step: String,
        /// Where it stands.
        state: StepState,
        /// The manifest's line for it, when it starts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// A line a step's tool printed.
    Log {
        /// The step's id.
        step: String,
        /// Which output.
        stream: Stream,
        /// The line, without its line break.
        line: String,
    },
    /// A ready condition was checked.
    Ready {
        /// The condition (`step_ok up`).
        condition: String,
        /// Where it stands.
        state: ReadyState,
        /// What was seen.
        detail: String,
    },
    /// An output's value, once the run is up.
    Output {
        /// The output's name.
        name: String,
        /// What it is for.
        role: OutputRole,
        /// Its value (`***` when sensitive), or null when it can't be read.
        value: Option<String>,
        /// Whether the value is sensitive.
        sensitive: bool,
    },
    /// One check's result.
    Check {
        /// The check runner's id.
        runner: String,
        /// Where it ran from.
        from: String,
        /// The check's name.
        name: String,
        /// Whether it passed.
        ok: bool,
        /// Why it failed (empty when it passed).
        detail: String,
    },
    /// Something the embedder may show, that didn't fail the operation.
    Warning {
        /// What kind.
        code: WarningCode,
        /// The text.
        message: String,
    },
    /// The operation's outcome, when it ran to its end.
    Result {
        /// Whether it reached its goal (false: a check failed).
        ok: bool,
        /// What it found or produced.
        data: serde_json::Value,
    },
    /// Why the operation failed.
    Error {
        /// The stable code.
        code: ErrorCode,
        /// The message.
        message: String,
        /// The step that failed, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<String>,
    },
}

/// One event of an operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Event {
    /// The stream's version ([`EVENTS_VERSION`]).
    pub v: u32,
    /// Its place in the operation, from 1.
    pub seq: u64,
    /// When (`YYYY-MM-DDTHH:MM:SSZ`, UTC).
    pub time: String,
    /// The run's name.
    pub run: String,
    /// The operation.
    pub op: Op,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// Where an operation's events go. Implemented for closures (`|e: &Event| …`).
pub trait Events: Send + Sync {
    /// Receives one event.
    fn emit(&self, event: &Event);
}

impl<F: Fn(&Event) + Send + Sync> Events for F {
    fn emit(&self, event: &Event) {
        self(event);
    }
}

/// Events written as JSON, one per line, flushed each time.
#[derive(Debug)]
pub struct JsonLines<W>(Mutex<W>);

impl<W: Write + Send> JsonLines<W> {
    /// Writes to `w`.
    pub fn new(w: W) -> JsonLines<W> {
        JsonLines(Mutex::new(w))
    }
}

impl<W: Write + Send> Events for JsonLines<W> {
    fn emit(&self, event: &Event) {
        let mut w = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let written = serde_json::to_string(event)
            .map_err(std::io::Error::from)
            .and_then(|text| writeln!(w, "{text}"))
            .and_then(|()| w.flush());
        // A closed output (a pipe whose reader left) mustn't stop the operation: the run's own log
        // keeps every event.
        if let Err(e) = written {
            tracing::debug!(error = %e, "an event wasn't written");
        }
    }
}

/// What kind of warning a `warning` event is, the same in the Rust API and the event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WarningCode {
    /// A step whose failure the manifest tolerates failed.
    StepTolerated,
    /// An embedder's hook failed.
    HookFailed,
    /// A check runner failed or ran out of time: its checks that didn't report fail.
    CheckRunnerFailed,
    /// The clean-up of a check runner that ran out of time failed.
    CheckCleanupFailed,
    /// What an earlier run left is removed before a start.
    LeftoversRemoved,
    /// A VM (or its folder) an earlier run left is removed.
    OrphanRemoved,
    /// The run last ran on another Vagrant provider: that run is cleared first.
    OtherProvider,
    /// A VM's saved state was cut off: it boots fresh.
    SavedStateDiscarded,
    /// A start failed on stale state: cleared, then started again.
    Recovering,
    /// An apply failed and is tried again.
    Retrying,
}

/// Events kept in memory, for an embedder that reads them later (and for tests).
#[derive(Debug, Default)]
pub struct Recorder(Mutex<Vec<Event>>);

impl Recorder {
    /// An empty recorder.
    #[must_use]
    pub fn new() -> Recorder {
        Recorder::default()
    }

    /// Every event so far, in order.
    pub fn events(&self) -> Vec<Event> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl Events for Recorder {
    fn emit(&self, event: &Event) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).push(event.clone());
    }
}

/// The JSON Schema of one event.
#[must_use]
pub fn events_schema() -> serde_json::Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(Event)).unwrap_or_default();
    if let Some(fields) = schema.as_object_mut() {
        fields.insert("title".to_owned(), serde_json::json!("Isothrum runner event, version 0"));
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind) -> Event {
        Event {
            v: 0,
            seq: 1,
            time: "2026-10-15T12:00:00Z".into(),
            run: "flag-portal".into(),
            op: Op::Up,
            kind,
        }
    }

    #[test]
    fn an_event_has_the_documented_shape() {
        let e = event(EventKind::Step {
            step: "up".into(),
            state: StepState::Running,
            note: Some("Starting…".into()),
        });
        assert_eq!(
            serde_json::to_value(&e).unwrap(),
            serde_json::json!({"v":0,"seq":1,"time":"2026-10-15T12:00:00Z","run":"flag-portal","op":"up","event":"step","step":"up","state":"running","note":"Starting…"})
        );
        let error = event(EventKind::Error {
            code: ErrorCode::PortInUse,
            message: "m".into(),
            step: None,
        });
        let back: Event = serde_json::from_value(serde_json::to_value(&error).unwrap()).unwrap();
        assert_eq!(back, error);
    }

    #[test]
    fn json_lines_are_one_event_per_line() {
        let sink = JsonLines::new(Vec::new());
        sink.emit(&event(EventKind::State {
            from: RunState::Absent,
            to: RunState::Starting,
        }));
        sink.emit(&event(EventKind::Result {
            ok: true,
            data: serde_json::json!({}),
        }));
        let text = String::from_utf8(sink.0.into_inner().unwrap()).unwrap();
        let lines: Vec<Event> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
    }

    #[test]
    fn the_schema_names_every_event_kind() {
        let schema = events_schema().to_string();
        for kind in ["state", "step", "log", "ready", "output", "check", "warning", "result", "error"] {
            assert!(schema.contains(&format!("\"{kind}\"")), "{kind}");
        }
    }
}
