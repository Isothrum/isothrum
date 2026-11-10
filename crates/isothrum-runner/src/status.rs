//! What a run looks like from outside: its state, machines, networks and outputs. Roles,
//! addresses and networks come from the manifest; the engine only says what runs and which host
//! ports it got.

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, OutputRole, Protocol, Role};
use serde::{Deserialize, Serialize};

use crate::events::MASK;
use crate::secrets::SecretValue;
use crate::state::RunState;
use crate::store::RunRecord;

/// A machine's state, as its engine reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MachineState {
    /// Running.
    Running,
    /// Running, but failing its health check.
    Unhealthy,
    /// Exited with this code (a job that finished, or a machine that died).
    Exited(i32),
    /// Created or stopped, not running.
    Stopped,
    /// Not there.
    Absent,
    /// Something else, as the engine says it.
    Unknown(String),
}

impl std::fmt::Display for MachineState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MachineState::Running => f.write_str("running"),
            MachineState::Unhealthy => f.write_str("unhealthy"),
            MachineState::Exited(code) => write!(f, "exited({code})"),
            MachineState::Stopped => f.write_str("stopped"),
            MachineState::Absent => f.write_str("absent"),
            MachineState::Unknown(s) => f.write_str(s),
        }
    }
}

/// A published port: where a machine's port answers on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct PublishedPort {
    /// The machine's port.
    pub port: u16,
    /// `tcp` or `udp`.
    pub protocol: Protocol,
    /// The host port.
    pub host_port: u16,
}

/// One machine of the manifest, as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct MachineStatus {
    /// Its name.
    pub name: String,
    /// What it is for.
    pub role: Role,
    /// Its state.
    pub state: MachineState,
    /// Its static addresses, by network.
    pub addresses: BTreeMap<String, String>,
    /// Its published ports.
    pub published: Vec<PublishedPort>,
}

/// A network of the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct NetworkView {
    /// Its name in the spec.
    pub name: String,
    /// Its block.
    pub cidr: String,
    /// Whether it reaches the internet.
    pub internet: bool,
    /// Its name where the target created it (`web-portal_backend`), when known.
    pub native: Option<String>,
}

/// An output's value. A sensitive one shows as `***` wherever it is written out (its JSON, its
/// `Debug`, an event); the runner keeps the value itself for its own use (a guest delivery's
/// address, masking it in what tools print).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct OutputValue {
    /// Its name.
    pub name: String,
    /// What it is for (`entry`: where a user starts, the address to open).
    pub role: OutputRole,
    /// Whether the value is sensitive.
    pub sensitive: bool,
    /// Its value, when it can be read; `***` when it is sensitive.
    pub value: Option<String>,
    /// A sensitive output's value itself: never written out.
    #[serde(skip)]
    secret: Option<SecretValue>,
}

impl OutputValue {
    /// Output `name`'s `value`, masked when it is `sensitive`.
    pub(crate) fn new(name: String, role: OutputRole, sensitive: bool, value: Option<String>) -> OutputValue {
        let (value, secret) = match value {
            Some(v) if sensitive => (Some(MASK.to_owned()), Some(SecretValue::new(v))),
            value => (value, None),
        };
        OutputValue {
            name,
            role,
            sensitive,
            value,
            secret,
        }
    }

    /// The value itself, a sensitive one unmasked. Never print or log it. `None` when it can't be
    /// read, and for a sensitive output read back from JSON (which holds only the mask).
    #[must_use]
    pub(crate) fn revealed(&self) -> Option<&str> {
        if self.sensitive {
            self.secret.as_ref().map(SecretValue::expose)
        } else {
            self.value.as_deref()
        }
    }
}

/// A run as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct Status {
    /// Where it stands.
    pub state: RunState,
    /// The Vagrant provider it uses, when it has one.
    pub provider: Option<String>,
    /// Every machine of the manifest.
    pub machines: Vec<MachineStatus>,
    /// The run's networks.
    pub networks: Vec<NetworkView>,
    /// Its outputs, read live.
    pub outputs: Vec<OutputValue>,
    /// The run's message, filled, once the run is ready.
    pub message: Option<String>,
}

/// The JSON Schema of a run's status (`isothrum status --json`).
#[must_use]
pub fn status_schema() -> serde_json::Value {
    crate::events::titled_schema::<Status>("Isothrum run status")
}

/// A run that came up (or resumed).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Started {
    /// Its record.
    pub record: RunRecord,
    /// Its outputs.
    pub outputs: Vec<OutputValue>,
    /// The run's message, filled (a sensitive output never substituted).
    pub message: Option<String>,
}

/// The state of a run whose machines are `machines`, given its record. Ready when every target
/// machine runs; parked when its record says so and none runs; absent when nothing is there.
pub(crate) fn derive_state(machines: &[MachineStatus], record: Option<&RunRecord>) -> RunState {
    let present = machines.iter().any(|m| m.state != MachineState::Absent);
    if !present {
        return RunState::Absent;
    }
    let targets: Vec<&MachineStatus> = machines.iter().filter(|m| m.role == Role::Target).collect();
    let running = |m: &&MachineStatus| m.state == MachineState::Running;
    if record.is_some_and(|r| r.state == RunState::Parked) && !targets.iter().any(running) {
        return RunState::Parked;
    }
    // A start that failed after its machines came up (a Terraform apply whose router never
    // reported its address, a set-up step that failed) left them running: the run isn't ready.
    if record.is_some_and(|r| r.state == RunState::Failed) {
        return RunState::Failed;
    }
    if !targets.is_empty() && targets.iter().all(running) {
        RunState::Ready
    } else {
        RunState::Failed
    }
}

/// Whether any target machine of `machines` runs.
pub(crate) fn any_target_running(machines: &[MachineStatus]) -> bool {
    machines
        .iter()
        .any(|m| m.role == Role::Target && matches!(m.state, MachineState::Running | MachineState::Unhealthy))
}

/// The manifest's networks, their native names filled by `native`.
pub(crate) fn networks(manifest: &Manifest, native: impl Fn(&isoloom_manifest::Template) -> Option<String>) -> Vec<NetworkView> {
    manifest
        .networks
        .iter()
        .map(|n| NetworkView {
            name: n.name.to_string(),
            cidr: n.cidr.to_string(),
            internet: n.internet,
            native: n.native.as_ref().and_then(&native),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine(role: Role, state: MachineState) -> MachineStatus {
        MachineStatus {
            name: "m".into(),
            role,
            state,
            addresses: BTreeMap::new(),
            published: Vec::new(),
        }
    }

    #[test]
    fn a_run_is_ready_when_every_target_runs() {
        use MachineState::{Absent, Exited, Running, Stopped};
        let all = |ms: Vec<MachineStatus>| derive_state(&ms, None);
        assert_eq!(all(vec![machine(Role::Target, Absent), machine(Role::Job, Absent)]), RunState::Absent);
        assert_eq!(
            all(vec![
                machine(Role::Target, Running),
                machine(Role::Job, Exited(0)),
                machine(Role::Infra, Running)
            ]),
            RunState::Ready
        );
        assert_eq!(all(vec![machine(Role::Target, Running), machine(Role::Target, Exited(1))]), RunState::Failed);
        assert_eq!(all(vec![machine(Role::Target, Stopped)]), RunState::Failed);
        assert_eq!(all(vec![machine(Role::Target, MachineState::Unhealthy)]), RunState::Failed);
    }

    #[test]
    fn a_parked_run_with_nothing_running_is_parked() {
        let mut record = crate::store::RunRecord::fresh(
            crate::name::RunName::new("r").unwrap(),
            "/x".into(),
            isoloom_manifest::Target::Docker,
            isoloom_manifest::Driver::Compose,
        );
        record.state = RunState::Parked;
        let stopped = vec![machine(Role::Target, MachineState::Exited(0))];
        assert_eq!(derive_state(&stopped, Some(&record)), RunState::Parked);
        assert!(!any_target_running(&stopped));
    }
}
