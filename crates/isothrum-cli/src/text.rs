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

/// A run's status as text: its state, then a line per machine, its outputs and message.
pub(crate) fn status_text(name: &str, status: &Status) -> Vec<String> {
    let mut lines = vec![format!("{name}: {}", status.state.as_str())];
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
}
