//! What a person reads: an event as a line, and a run's status as a short table.

use isothrum_runner::{EventKind, MachineState, ReadyState, Status, StepState};

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
        EventKind::Ready {
            condition,
            state: ReadyState::Ok,
            ..
        } => format!("ready: {condition}"),
        EventKind::Ready {
            condition,
            state: ReadyState::Failed,
            detail,
        } => format!("not ready: {condition}: {detail}"),
        EventKind::Ready {
            condition,
            state: ReadyState::Waiting,
            detail,
        } if !detail.is_empty() => format!("waiting: {condition}: {detail}"),
        EventKind::Output { name, value: Some(value), .. } => format!("{name}: {value}"),
        EventKind::Check { name, from, ok, detail, .. } => {
            let verdict = if *ok { "PASS".to_owned() } else { format!("FAIL: {detail}") };
            format!("check {name} ({from}): {verdict}")
        }
        EventKind::Warning { message, .. } => format!("warning: {message}"),
        EventKind::Error { message, .. } => format!("error: {message}"),
        _ => return None,
    })
}

/// A run's status as text: its state, then a line per machine, its outputs and message. While an
/// operation holds the run, its first line says the machines are as last read.
pub(crate) fn status_text(name: &str, status: &Status) -> Vec<String> {
    let held = if status.live {
        ""
    } else {
        " (an operation is in progress: machines as last read)"
    };
    let mut lines = vec![format!("{name}: {}{held}", status.state.as_str())];
    for m in &status.machines {
        if m.state == MachineState::Absent && status.machines.iter().any(|o| o.state != MachineState::Absent) {
            continue;
        }
        let role = isothrum_runner::wire_name(&m.role);
        let ports: Vec<String> = m.published.iter().map(|p| format!("{}->{}", p.host_port, p.port)).collect();
        lines.push(
            format!("  {:<24} {:<7} {:<12} {}", m.name, role, m.state.to_string(), ports.join(" "))
                .trim_end()
                .to_owned(),
        );
    }
    for o in &status.outputs {
        if let Some(value) = &o.value {
            lines.push(format!("{}: {value}", o.name));
        }
    }
    if let Some(message) = &status.message {
        lines.push(message.trim_end().to_owned());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_sees_notes_logs_checks_and_errors() {
        let step = EventKind::Step {
            step: "up".into(),
            state: StepState::Running,
            note: Some("Starting…".into()),
        };
        assert_eq!(human(&step).as_deref(), Some("==> Starting…"));
        assert_eq!(
            human(&EventKind::Step {
                step: "up".into(),
                state: StepState::Done,
                note: None
            }),
            None
        );
        let check = EventKind::Check {
            runner: "r".into(),
            from: "from web".into(),
            name: "db:3306".into(),
            ok: false,
            detail: "refused".into(),
        };
        assert_eq!(human(&check).as_deref(), Some("check db:3306 (from web): FAIL: refused"));
    }

    #[test]
    fn a_status_read_while_an_operation_holds_the_run_says_its_machines_are_as_last_read() {
        let status = |live: bool| -> Status {
            serde_json::from_value(serde_json::json!({
                "state": "starting", "live": live, "provider": null, "machines": [], "networks": [], "outputs": [], "message": null
            }))
            .unwrap()
        };
        assert_eq!(
            status_text("demo", &status(false))[0],
            "demo: starting (an operation is in progress: machines as last read)"
        );
        assert_eq!(status_text("demo", &status(true))[0], "demo: starting");
    }

    /// A wait that says what it waits for is shown as it goes, not only once it ends.
    #[test]
    fn a_person_sees_a_wait_that_says_why() {
        let waiting = |detail: &str| EventKind::Ready {
            condition: "ready_file 203.0.113.7".into(),
            state: ReadyState::Waiting,
            detail: detail.into(),
        };
        assert_eq!(
            human(&waiting("running: docker compose up")).as_deref(),
            Some("waiting: ready_file 203.0.113.7: running: docker compose up")
        );
        assert_eq!(human(&waiting("")), None, "nothing to say");
    }
}
