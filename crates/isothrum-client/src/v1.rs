//! The shared types of the 3.0 state model the protocol carries (`OperationId`, `EventCursor`,
//! events v1, the operation row), as the service needs them before `isothrum_runner::store`
//! defines them. Their wire forms are the store's: once the runner exports these, this module
//! re-exports the runner's and the protocol doesn't change.

use std::fmt;

use isothrum_runner::{ErrorCode, Op, RunName, RunRecord};
use serde::{Deserialize, Serialize};

/// An operation's id: increasing per home, never reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(pub i64);

impl fmt::Display for OperationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// An event's place in the home's event stream, increasing across every run and operation: a
/// subscriber resumes after the last one it saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventCursor(pub u64);

/// Where an operation stands, as its `operation` events say.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum OperationPhase {
    /// Queued behind others of its run.
    Queued,
    /// Begun.
    Started,
    /// Begun again, recovering an operation whose process died.
    Resumed,
    /// Ended (its `result` or `error` came before).
    Ended,
}

/// One event of the stream (v1): the runner's v0 event, with the operation's journal id, and the
/// `operation` events the service writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Event {
    /// The stream's version.
    pub v: u32,
    /// Its place in the operation, from 1.
    pub seq: u64,
    /// When (`YYYY-MM-DDTHH:MM:SSZ`, UTC).
    pub time: String,
    /// The run's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    /// The operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<Op>,
    /// The operation's id: on every event of an operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationId>,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
}

/// The stream's version these events are.
pub const EVENTS_VERSION: u32 = 1;

impl Event {
    /// The runner's event `event`, of operation `operation`.
    #[must_use]
    pub fn of_runner(event: &isothrum_runner::Event, operation: Option<OperationId>) -> Event {
        Event {
            v: EVENTS_VERSION,
            seq: event.seq,
            time: event.time.clone(),
            run: event.run.clone(),
            op: event.op,
            operation,
            kind: EventKind::Runner(event.kind.clone()),
        }
    }

    /// An `operation` event of `operation` (`op` on `run`): the service's own, numbered 0 in it.
    #[must_use]
    pub fn phase(run: &RunName, op: Op, operation: OperationId, phase: OperationPhase, position: Option<u32>) -> Event {
        Event {
            v: EVENTS_VERSION,
            seq: 0,
            time: isothrum_runner::rfc3339(std::time::SystemTime::now()),
            run: Some(run.to_string()),
            op: Some(op),
            operation: Some(operation),
            kind: EventKind::Operation(OperationEvent::Operation { phase, position }),
        }
    }

    /// The phase it says, when it is an `operation` event.
    #[must_use]
    pub fn phase_of(&self) -> Option<OperationPhase> {
        match &self.kind {
            EventKind::Operation(OperationEvent::Operation { phase, .. }) => Some(*phase),
            EventKind::Runner(_) => None,
        }
    }

    /// Whether it ends its operation's work: its `result` or `error`.
    #[must_use]
    pub fn is_outcome(&self) -> bool {
        matches!(
            self.kind,
            EventKind::Runner(isothrum_runner::EventKind::Result { .. } | isothrum_runner::EventKind::Error { .. })
        )
    }

    /// The runner's form of it (v0), when the runner wrote it.
    #[must_use]
    pub fn to_runner(&self) -> Option<isothrum_runner::Event> {
        let EventKind::Runner(kind) = &self.kind else { return None };
        let mut event = isothrum_runner::Event::new(self.seq, self.run.clone(), self.op, kind.clone());
        event.time.clone_from(&self.time);
        Some(event)
    }
}

/// What happened: the service's `operation` events, else the runner's kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum EventKind {
    /// An operation was queued, began or ended.
    Operation(OperationEvent),
    /// The runner's: a state, a step, a log line, an outcome…
    Runner(isothrum_runner::EventKind),
}

/// The `operation` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum OperationEvent {
    /// An operation moved.
    Operation {
        /// Where it stands.
        phase: OperationPhase,
        /// Queued: how many of its run's operations are ahead of it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        position: Option<u32>,
    },
}

/// How an operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// It reached its goal.
    Succeeded,
    /// It failed.
    Failed,
    /// It was cancelled.
    Cancelled,
    /// Its process died before it ended.
    Abandoned,
}

/// When and how an operation ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// When.
    pub at: String,
    /// How.
    pub outcome: Outcome,
    /// Its error's code, when it failed.
    pub code: Option<ErrorCode>,
}

/// An operation of the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OperationRow {
    /// Its id.
    pub id: OperationId,
    /// Its run.
    pub run: RunName,
    /// What it does.
    pub op: Op,
    /// When it was accepted.
    pub started_at: String,
    /// None while it is queued or runs.
    pub ended: Option<Ended>,
    /// Whether a cancel was asked.
    pub cancel_requested: bool,
}

impl OperationRow {
    /// Operation `id`, `op` on `run`, accepted now.
    #[must_use]
    pub fn new(id: OperationId, run: RunName, op: Op) -> OperationRow {
        OperationRow {
            id,
            run,
            op,
            started_at: isothrum_runner::rfc3339(std::time::SystemTime::now()),
            ended: None,
            cancel_requested: false,
        }
    }
}

/// A run as `list` gives it: its record.
pub type RunRow = RunRecord;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_event_and_a_runner_event_read_back_as_themselves() {
        let run = RunName::new("web").unwrap();
        let queued = Event::phase(&run, Op::Up, OperationId(3), OperationPhase::Queued, Some(1));
        let line = serde_json::to_string(&queued).unwrap();
        assert!(line.contains(r#""operation":3,"event":"operation","phase":"queued","position":1"#), "{line}");
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), queued);

        let kind = isothrum_runner::EventKind::Result {
            ok: true,
            data: serde_json::Value::Null,
        };
        let runner = isothrum_runner::Event::new(1, Some("web".into()), Some(Op::Up), kind);
        let wrapped = Event::of_runner(&runner, Some(OperationId(3)));
        let wrapped_line = serde_json::to_string(&wrapped).unwrap();
        assert!(
            wrapped_line.contains(r#""v":1"#) && wrapped_line.contains(r#""event":"result""#),
            "{wrapped_line}"
        );
        let back = serde_json::from_str::<Event>(&wrapped_line).unwrap();
        assert!(back.is_outcome() && back.phase_of().is_none());
        assert_eq!(back.to_runner().unwrap().kind, runner.kind);
    }
}
