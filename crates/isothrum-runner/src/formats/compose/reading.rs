//! What the driver makes of what Compose and the engine say: the run's state from its services, a found object's key, a command's outcome.

use super::{MachineState, Object, Ran, RunState, SeenMachine};

/// The run's state as `machines` show it: absent when nothing is there; parked when nothing runs;
/// ready when every service that runs is up and healthy and every one that ended ended well (an
/// init job); failed otherwise (one unhealthy, one that died, one missing among running ones).
pub(super) fn run_state(machines: &[SeenMachine]) -> RunState {
    if machines.is_empty() || machines.iter().all(|m| m.state == MachineState::Absent) {
        return RunState::Absent;
    }
    if !machines.iter().any(|m| m.state.runs()) {
        return RunState::Parked;
    }
    let fine = |m: &SeenMachine| matches!(m.state, MachineState::Running | MachineState::Exited(0));
    if machines.iter().all(fine) { RunState::Ready } else { RunState::Failed }
}

/// The services a `retire` or `attach` names, refused when `machine` isn't one of the project's.
pub(super) fn one_service(machine: &str) -> Vec<String> {
    vec![machine.to_owned()]
}

/// The word a [`Found`] key starts with for `object`.
pub(super) fn object_word(object: Object) -> &'static str {
    match object {
        Object::Container => "container",
        Object::Network => "network",
        Object::Volume => "volume",
    }
}

/// A command's outcome as [`Ran`]: a command that ran and failed is its exit status and what it
/// printed, not an error.
pub(super) fn ran(captured: &crate::process::Captured) -> Ran {
    Ran {
        exit: captured.exit.code.unwrap_or(-1),
        stdout: captured.stdout.join("\n"),
        stderr: captured.stderr_tail.join("\n"),
    }
}
