//! What a person reads: an event as a line, and a run's status as a short table.

use isothrum_runner::{EventKind, MachineState, Status, StepState};

/// An event as a line of text; `None` for those a person needn't see.
pub(crate) fn human(kind: &EventKind) -> Option<String> {
    Some(match kind {
        EventKind::Step {
            step,
            state: StepState::Running,
            note,
        } => format!("==> {}", note.as_deref().unwrap_or(step)),
        EventKind::Step {
            step,
            state: StepState::Failed,
            ..
        } => format!("==> {step}: failed"),
        EventKind::Step {
            step,
            state: StepState::Skipped,
            ..
        } => format!("==> {step}: skipped"),
        EventKind::Log { line, .. } => format!("    {line}"),
        EventKind::Output { name, value: Some(value), .. } => format!("{name}: {value}"),
        EventKind::Warning { message, .. } => format!("warning: {message}"),
        EventKind::Error(report) => format!("error: {}", report.message),
        _ => return None,
    })
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
        let step = |state: StepState, note: Option<&str>| EventKind::Step {
            step: "up".into(),
            state,
            note: note.map(Into::into),
        };
        let event = |json: serde_json::Value| -> EventKind { serde_json::from_value(json).unwrap() };
        for (kind, line) in [
            (step(StepState::Running, Some("Starting…")), Some("==> Starting…")),
            (step(StepState::Running, None), Some("==> up")),
            (step(StepState::Failed, None), Some("==> up: failed")),
            (step(StepState::Skipped, None), Some("==> up: skipped")),
            (step(StepState::Done, None), None),
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
