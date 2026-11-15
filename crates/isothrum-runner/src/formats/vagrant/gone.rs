//! A machine destroyed outside the run (`vagrant destroy -f web` in a terminal, the VM deleted in
//! the hypervisor's app) and one never made (`autostart: false`, a machine only an `attach`
//! starts) both read `not_created`, and Vagrant forgets the first one's id at its next look. What
//! tells them apart is the store: a machine whose VM it records as made (its resource row, from the
//! operation that made it) is gone; one it records nothing of was never made.

use super::{Error, Result, SeenMachine, ToolCx, Vagrant, names::of_key};
use crate::state::RunState;
use crate::status::MachineState;

/// The machines of `states` (each one's state label, from `status`) that read `not_created`
/// though the store records their VM as made on the run's provider: destroyed outside the run.
pub(super) fn gone(cx: &ToolCx<'_>, states: &std::collections::BTreeMap<String, String>) -> Result<Vec<String>> {
    let made: Vec<(String, Option<String>)> = (cx.made)()?.iter().filter_map(|key| of_key(key, cx.provider)).collect();
    let asked = cx.provider.map(|p| p.id().to_owned());
    Ok(states
        .iter()
        .filter(|(_, label)| crate::tool_output::vagrant::machine_state(label) == MachineState::Absent)
        .filter(|(name, _)| {
            made.iter()
                .any(|(machine, provider)| machine == *name && (asked.is_none() || *provider == asked))
        })
        .map(|(name, _)| name.clone())
        .collect())
}

impl Vagrant {
    /// Refuses a resume of a run a machine of which is gone: a VM destroyed comes back from its box
    /// only, never as it was kept, so the run is started again with `up`. A run the store records
    /// nothing made of has nothing gone, and isn't read.
    pub(super) async fn refuse_resume_if_gone(&self, cx: &ToolCx<'_>) -> Result<()> {
        if (cx.made)()?.is_empty() {
            return Ok(());
        }
        let gone = gone(cx, &self.status(cx).await?)?;
        if gone.is_empty() {
            return Ok(());
        }
        Err(Error::PartlyGone {
            run: cx.run.to_string(),
            machines: gone,
        })
    }
}

/// The run's state as `machines` show it, `gone` of them destroyed outside the run ([`gone`]):
/// absent when none was made; failed when one made is gone; parked when none of those made runs;
/// ready when each one made runs or is stopped (a machine its own provisioner powers off, such as
/// a lab's controller, among running ones: it reads stopped); failed otherwise (one unhealthy,
/// aborted, or in a state of its own). A machine never made (one the Vagrantfile leaves down at
/// `up`, `autostart: false`) doesn't count: a start that failed midway fails its own operation.
pub(super) fn run_state(machines: &[SeenMachine], gone: &[String]) -> RunState {
    if !gone.is_empty() {
        return RunState::Failed;
    }
    let made: Vec<&SeenMachine> = machines.iter().filter(|m| m.state != MachineState::Absent).collect();
    if made.is_empty() {
        return RunState::Absent;
    }
    if !made.iter().any(|m| m.state.runs()) {
        return RunState::Parked;
    }
    if made.iter().all(|m| matches!(m.state, MachineState::Running | MachineState::Stopped)) {
        RunState::Ready
    } else {
        RunState::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_state_rule() {
        let m = |name: &str, state: MachineState| SeenMachine {
            name: name.into(),
            state,
            published: Vec::new(),
            kind: None,
            address: None,
            windows: false,
        };
        assert_eq!(run_state(&[], &[]), RunState::Absent);
        assert_eq!(run_state(&[m("a", MachineState::Absent)], &[]), RunState::Absent);
        assert_eq!(
            run_state(&[m("a", MachineState::Stopped), m("b", MachineState::Stopped)], &[]),
            RunState::Parked
        );
        assert_eq!(
            run_state(&[m("a", MachineState::Running), m("attack", MachineState::Absent)], &[]),
            RunState::Ready,
            "a machine left down at up (autostart: false) isn't a failure"
        );
        assert_eq!(
            run_state(&[m("a", MachineState::Running), m("controller", MachineState::Stopped)], &[]),
            RunState::Ready,
            "a machine that powered itself off among running ones isn't a failure"
        );
        assert_eq!(
            run_state(&[m("a", MachineState::Running), m("b", MachineState::Unknown("aborted".into()))], &[]),
            RunState::Failed
        );
        assert_eq!(
            run_state(&[m("a", MachineState::Running), m("b", MachineState::Unknown("shut".into()))], &[]),
            RunState::Failed
        );
        let web = || vec!["web".to_owned()];
        assert_eq!(
            run_state(&[m("web", MachineState::Absent)], &web()),
            RunState::Failed,
            "the run's one machine destroyed outside it fails it, never reads as never made"
        );
    }
}
