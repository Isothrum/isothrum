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

impl MachineState {
    /// Whether the machine runs: up, whether or not it passes its health check (the one rule for
    /// "it runs", however it is asked).
    pub(crate) fn runs(&self) -> bool {
        matches!(self, MachineState::Running | MachineState::Unhealthy)
    }
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
    /// Where it stands. While an operation holds the run: the moving state it is in (`starting`
    /// through an `up`), else, before it records one or for an operation that leaves the run as
    /// it was meanwhile (an `update`: `ready`), the state the run's record holds.
    pub state: RunState,
    /// Whether the machines and outputs were read from the engine now. False while an operation
    /// holds the run (in this process or another), whose tools a read would contend with (Vagrant
    /// refuses to read a VM's state while a `vagrant up` provisions it): they are then as the
    /// runner last read them (none before it ever did), and the outputs the engine itself gives
    /// are left out.
    #[serde(default = "live_when_absent")]
    pub live: bool,
    /// The Vagrant provider it uses, when it has one.
    pub provider: Option<String>,
    /// Every machine of the manifest.
    pub machines: Vec<MachineStatus>,
    /// The run's networks.
    pub networks: Vec<NetworkView>,
    /// Its outputs, each filled from what was read. While not [`Status::live`], an output the
    /// engine itself gives (a Terraform output) has only its default, if any.
    pub outputs: Vec<OutputValue>,
    /// The run's message, filled, once the run is ready.
    pub message: Option<String>,
}

impl Status {
    /// Whether anything of the run is there: a machine that isn't absent.
    #[must_use]
    pub fn any_present(&self) -> bool {
        any_present(&self.machines)
    }
}

/// [`Status::live`] in a status written before it existed: every status was read from the
/// engine then.
fn live_when_absent() -> bool {
    true
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

/// The state of a run whose machines are `machines`, given its record. Absent when nothing is
/// there; parked when its record says so and none runs ([`any_member_running`]: one up with a
/// failing healthcheck runs); failed when its record says its start failed, whatever runs; ready
/// when every member machine runs healthy; failed otherwise.
pub(crate) fn derive_state(machines: &[MachineStatus], record: Option<&RunRecord>) -> RunState {
    if !any_present(machines) {
        return RunState::Absent;
    }
    if record.is_some_and(|r| r.state == RunState::Parked) && !any_member_running(machines) {
        return RunState::Parked;
    }
    let members: Vec<&MachineStatus> = machines.iter().filter(|m| m.role == Role::Member).collect();
    let running = |m: &&MachineStatus| m.state == MachineState::Running;
    // A start that failed after its machines came up (a Terraform apply whose router never
    // reported its address, a set-up step that failed) left them running: the run isn't ready.
    if record.is_some_and(|r| r.state == RunState::Failed) {
        return RunState::Failed;
    }
    if !members.is_empty() && members.iter().all(running) {
        RunState::Ready
    } else {
        RunState::Failed
    }
}

/// Whether anything of the run is there: a machine of `machines` that isn't absent.
pub(crate) fn any_present(machines: &[MachineStatus]) -> bool {
    machines.iter().any(|m| m.state != MachineState::Absent)
}

/// Whether any member machine of `machines` runs.
pub(crate) fn any_member_running(machines: &[MachineStatus]) -> bool {
    machines.iter().any(|m| m.role == Role::Member && m.state.runs())
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
    fn a_run_is_ready_when_every_member_runs() {
        use MachineState::{Absent, Exited, Running, Stopped};
        let all = |ms: Vec<MachineStatus>| derive_state(&ms, None);
        assert_eq!(all(vec![machine(Role::Member, Absent), machine(Role::Job, Absent)]), RunState::Absent);
        assert_eq!(
            all(vec![
                machine(Role::Member, Running),
                machine(Role::Job, Exited(0)),
                machine(Role::Infra, Running)
            ]),
            RunState::Ready
        );
        assert_eq!(all(vec![machine(Role::Member, Running), machine(Role::Member, Exited(1))]), RunState::Failed);
        assert_eq!(all(vec![machine(Role::Member, Stopped)]), RunState::Failed);
        assert_eq!(all(vec![machine(Role::Member, MachineState::Unhealthy)]), RunState::Failed);
    }

    #[test]
    fn a_status_from_before_live_reads_as_live() {
        let json = r#"{"state":"ready","provider":null,"machines":[],"networks":[],"outputs":[],"message":null}"#;
        let status: Status = serde_json::from_str(json).unwrap();
        assert!(status.live);
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
        let stopped = vec![machine(Role::Member, MachineState::Exited(0))];
        assert_eq!(derive_state(&stopped, Some(&record)), RunState::Parked);
        assert!(!any_member_running(&stopped));
        // Regression: a member up with a failing healthcheck runs, as `check` and `provision`
        // see it, so the run isn't parked (nor ready).
        let unhealthy = vec![machine(Role::Member, MachineState::Unhealthy)];
        assert!(any_member_running(&unhealthy));
        assert_eq!(derive_state(&unhealthy, Some(&record)), RunState::Failed);
    }
}
