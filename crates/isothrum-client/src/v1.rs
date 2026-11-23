//! The shared types of the 3.0 state model the protocol carries, as the runner defines them
//! (`OperationId`, `EventCursor`, events v1, the journal's rows), and what a client reads of an
//! event.

pub use isothrum_runner::store::{Ended, EventCursor, OperationId, OperationRow, Outcome, RunRow};
pub use isothrum_runner::{Event, EventKind, OperationPhase};

use isothrum_runner::{Op, RunName};

/// The `operation` event saying `phase` of `operation` (`op` on `run`): the service's own,
/// numbered 0 in it, with its queue `position` when queued and its `outcome` when ended.
#[must_use]
pub fn phase_event(run: &RunName, (op, operation): (Op, OperationId), phase: OperationPhase, position: Option<u32>, outcome: Option<Outcome>) -> Event {
    let mut event = Event::new(0, Some(run.to_string()), Some(op), EventKind::Operation { phase, outcome, position });
    event.operation = Some(operation);
    event
}

/// The phase `event` says, when it is an `operation` event.
#[must_use]
pub fn phase_of(event: &Event) -> Option<OperationPhase> {
    match &event.kind {
        EventKind::Operation { phase, .. } => Some(*phase),
        _ => None,
    }
}

/// Whether `event` ends its operation's work: its `result` or `error`.
#[must_use]
pub fn is_outcome(event: &Event) -> bool {
    matches!(event.kind, EventKind::Result { .. } | EventKind::Error { .. })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operation_event_reads_back_as_itself() {
        let run = RunName::new("web").unwrap();
        let queued = phase_event(&run, (Op::Up, OperationId(3)), OperationPhase::Queued, Some(1), None);
        let line = serde_json::to_string(&queued).unwrap();
        let fields: serde_json::Value = serde_json::from_str(&line).unwrap();
        let said = |key: &str| fields.get(key).cloned().unwrap_or_default();
        assert_eq!(
            (said("operation"), said("event"), said("phase"), said("position")),
            (3.into(), "operation".into(), "queued".into(), 1.into()),
            "{line}"
        );
        let back = serde_json::from_str::<Event>(&line).unwrap();
        assert_eq!(phase_of(&back), Some(OperationPhase::Queued));
        assert!(!is_outcome(&back));
    }
}
