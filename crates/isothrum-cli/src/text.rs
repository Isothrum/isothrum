//! What a person reads: an event as a line, and a run's status as a short table.

use isothrum_runner::{EventKind, MachineState, Status, StepCount, StepState};

/// An event as a line of text; `None` for those a person needn't see.
pub(crate) fn human(kind: &EventKind) -> Option<String> {
    Some(match kind {
        EventKind::Step {
            step,
            state: StepState::Started,
            label,
            count,
        } => format!("==> {}", step_said(step, label.as_deref(), *count)),
        EventKind::Step {
            step,
            state: StepState::Failed,
            label,
            ..
        } => format!("==> {}: failed", step_said(step, label.as_deref(), None)),
        EventKind::Step {
            step,
            state: StepState::Skipped,
            label,
            ..
        } => format!("==> {}: skipped", step_said(step, label.as_deref(), None)),
        EventKind::Log { line, .. } => format!("    {line}"),
        EventKind::Output { name, value: Some(value), .. } => format!("{name}: {value}"),
        EventKind::Warning { message, .. } => format!("warning: {message}"),
        EventKind::Error(report) => format!("error: {}", report.message),
        _ => return None,
    })
}

/// A step as a person reads it: its id, what it is about, how far it is (`up dc01`, `images 1/3`).
fn step_said(step: &str, label: Option<&str>, count: Option<StepCount>) -> String {
    let label = label.map(|l| format!(" {l}")).unwrap_or_default();
    let count = count
        .map(|c| match c.total {
            Some(total) => format!(" {}/{total}", c.done),
            None => format!(" {}", c.done),
        })
        .unwrap_or_default();
    format!("{step}{label}{count}")
}

/// A run's status as text: its state, then a line per machine and its outputs. While an
/// operation holds the run, its first line says the machines are as last read.
pub(crate) fn status_text(name: &str, status: &Status) -> Vec<String> {
    let held = if status.live {
        ""
    } else {
        " (an operation is in progress: machines as last read)"
    };
    let mut lines = vec![format!("{name}: {}{held}", status.state.as_str())];
    // Once anything of the run is there, a machine that isn't goes unsaid.
    let any_present = status.any_present();
    for m in &status.machines {
        if m.state == MachineState::Absent && any_present {
            continue;
        }
        let ports: Vec<String> = m
            .published
            .iter()
            .map(|p| match p.protocol {
                Some(protocol) => format!("{}->{}/{protocol}", p.host_port, p.port),
                None => format!("{}->{}", p.host_port, p.port),
            })
            .collect();
        let at = m.address.as_deref().map(|a| format!("at {a} ")).unwrap_or_default();
        lines.push(
            format!("  {:<24} {:<12} {at}{}", m.name, m.state.to_string(), ports.join(" "))
                .trim_end()
                .to_owned(),
        );
    }
    for o in &status.outputs {
        if let Some(value) = &o.value {
            lines.push(format!("{}: {value}", o.name));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_sees_steps_logs_outputs_warnings_and_errors_and_nothing_else() {
        let step = |state: StepState, label: Option<&str>, count: Option<StepCount>| EventKind::Step {
            step: "up".into(),
            state,
            label: label.map(Into::into),
            count,
        };
        let event = |json: serde_json::Value| -> EventKind { serde_json::from_value(json).unwrap() };
        for (kind, line) in [
            (step(StepState::Started, Some("dc01"), None), Some("==> up dc01")),
            (step(StepState::Started, None, Some(StepCount::new(1, Some(3)))), Some("==> up 1/3")),
            (step(StepState::Started, None, Some(StepCount::new(2, None))), Some("==> up 2")),
            (step(StepState::Failed, Some("dc01"), None), Some("==> up dc01: failed")),
            (step(StepState::Skipped, None, None), Some("==> up: skipped")),
            (step(StepState::Done, None, None), None),
            (
                event(serde_json::json!({"event": "log", "step": "up", "stream": "stdout", "line": "pulled"})),
                Some("    pulled"),
            ),
            (
                event(serde_json::json!({"event": "warning", "code": "step_tolerated", "message": "slow"})),
                Some("warning: slow"),
            ),
            (
                event(serde_json::json!({"event": "output", "name": "url", "value": "http://web", "sensitive": false})),
                Some("url: http://web"),
            ),
            (
                event(serde_json::json!({"event": "output", "name": "url", "value": null, "sensitive": false})),
                None,
            ),
            (
                event(serde_json::json!({"event": "error", "code": "tool_failed", "message": "docker failed"})),
                Some("error: docker failed"),
            ),
        ] {
            assert_eq!(human(&kind).as_deref(), line, "{kind:?}");
        }
    }

    #[test]
    fn a_status_read_while_an_operation_holds_the_run_says_its_machines_are_as_last_read() {
        let status = |live: bool| -> Status {
            serde_json::from_value(serde_json::json!({
                "state": "creating", "live": live, "provider": null, "machines": [], "outputs": []
            }))
            .unwrap()
        };
        assert_eq!(
            status_text("demo", &status(false))[0],
            "demo: creating (an operation is in progress: machines as last read)"
        );
        assert_eq!(status_text("demo", &status(true))[0], "demo: creating");
    }
}
