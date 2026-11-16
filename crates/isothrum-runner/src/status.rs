//! What a run looks like from outside: its state, its machines and the host ports they publish,
//! and the outputs the project gives back, each as its tool reports it. What a machine or a port
//! is for is the embedder's to know, from the project's own description.

use serde::{Deserialize, Serialize};

use crate::events::MASK;
use crate::project::platform::{Protocol, Provider};
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
    /// Not read: another process acts on it (Vagrant's lock on the machine, held by a `vagrant
    /// up` that isn't this runner's), so its state is whatever that action leaves.
    Busy,
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
            MachineState::Busy => f.write_str("busy"),
        }
    }
}

/// A published port: where a machine's port answers on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct PublishedPort {
    /// The machine's port, inside it.
    pub port: crate::port::GuestPort,
    /// `tcp` or `udp`; `None` when the tool doesn't say (Vagrant's `port`), never guessed.
    pub protocol: Option<Protocol>,
    /// The host port it is reached at, at the machine's host address.
    pub host_port: crate::port::HostPort,
}

/// One machine of the run, as its tool reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct MachineStatus {
    /// Its name in the project (a Compose service, a Vagrant machine, a module's VM).
    pub name: String,
    /// Its state.
    pub state: MachineState,
    /// Its published ports.
    pub published: Vec<PublishedPort>,
    /// The address it is reached at itself, when its tool says one another computer reaches (a
    /// VM on a bridged or host-only network, a server's VM, a cloud instance); none for one
    /// reached only through its published ports. On ESXi, which forwards no port to this
    /// computer, a machine's ports answer at this address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Its graphical console, as its hypervisor offers it; none for a machine with no screen to
    /// speak of (a container), and while not [`Status::live`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<crate::graphical::ConsoleOffer>,
}

/// An output's value (a Terraform module's output). A sensitive one shows as `***` wherever it
/// is written out (its JSON, its `Debug`, an event); the runner keeps the value itself for the
/// embedder ([`OutputValue::revealed`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct OutputValue {
    /// Its name.
    pub name: String,
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
    pub(crate) fn new(name: String, sensitive: bool, value: Option<String>) -> OutputValue {
        let (value, secret) = match value {
            Some(v) if sensitive => (Some(MASK.to_owned()), Some(SecretValue::new(v))),
            value => (value, None),
        };
        OutputValue {
            name,
            sensitive,
            value,
            secret,
        }
    }

    /// The same, a sensitive one with its own value instead of the mask (a status read asked to
    /// reveal it, [`crate::RunOptions::reveal`]).
    #[must_use]
    pub(crate) fn unmasked(mut self) -> OutputValue {
        if let Some(secret) = &self.secret {
            self.value = Some(secret.expose().to_owned());
        }
        self
    }

    /// The value itself, a sensitive one unmasked. Never print or log it. `None` when it can't be
    /// read, and for a sensitive output read back from JSON (which holds only the mask).
    #[must_use]
    pub fn revealed(&self) -> Option<&str> {
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
    /// Where it stands. While an operation holds the run: the moving state it is in (`creating`
    /// through an `up`), else, before it records one or for an operation that leaves the run as
    /// it was meanwhile (an `update`: `ready`), the state the run's record holds.
    pub state: RunState,
    /// Whether the machines and outputs were read from the engine now. False while an operation
    /// holds the run (in this process or another), whose tools a read would contend with (Vagrant
    /// refuses to read a VM's state while a `vagrant up` provisions it): they are then as the
    /// runner last read them (none before it ever did), and the outputs the engine itself gives
    /// are left out.
    #[serde(default = "default_live")]
    pub live: bool,
    /// What the user wants of the run (the last operation that set it: `up`, `park`, `down`);
    /// none for a run never recorded. With `state`, whether the run is where it should be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired: Option<crate::state::Desired>,
    /// Why the run failed, when its recorded state is `failed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_cause: Option<crate::ErrorCode>,
    /// The Vagrant provider it uses, when it has one.
    pub provider: Option<Provider>,
    /// Every machine its tool reports.
    pub machines: Vec<MachineStatus>,
    /// What the project gives back (a module's outputs), as read; none while not
    /// [`Status::live`].
    pub outputs: Vec<OutputValue>,
    /// The hypervisor's own app the run's VMs open in ([`crate::Running::native_console`]); none
    /// where there is none (containers, QEMU), and while not [`Status::live`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<crate::graphical::NativeApp>,
}

impl Status {
    /// Whether anything of the run is there: a machine that isn't absent.
    #[must_use]
    pub fn any_present(&self) -> bool {
        any_present(&self.machines)
    }
}

/// [`Status::live`]'s default, for a status written before the field existed: every status was
/// read from the engine then.
fn default_live() -> bool {
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
}

/// Whether anything of the run is there: a machine of `machines` that isn't absent.
pub(crate) fn any_present(machines: &[MachineStatus]) -> bool {
    machines.iter().any(|m| m.state != MachineState::Absent)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The JSON `isothrum status --json` prints and an embedder reads: a machine's state by its
    /// name (an exit with its code), a port with no protocol said as null, the optional fields
    /// left out when unset; and it reads back as itself.
    #[test]
    fn a_status_is_written_as_its_schema_documents_and_reads_back() {
        let machine = |name: &str, state: MachineState| MachineStatus {
            name: name.into(),
            state,
            published: Vec::new(),
            address: None,
            console: None,
        };
        let mut web = machine("web", MachineState::Running);
        web.published = vec![PublishedPort {
            port: crate::port::GuestPort::new(80),
            protocol: Some(Protocol::Tcp),
            host_port: crate::port::HostPort::new(41001),
        }];
        let mut vm = machine("vm", MachineState::Unknown("saved".into()));
        vm.published = vec![PublishedPort {
            port: crate::port::GuestPort::new(22),
            protocol: None,
            host_port: crate::port::HostPort::new(2222),
        }];
        vm.address = Some("192.0.2.10".into());
        let status = Status {
            state: RunState::Ready,
            live: true,
            desired: Some(crate::state::Desired::Running),
            failed_cause: None,
            provider: Some(Provider::Virtualbox),
            machines: vec![web, machine("job", MachineState::Exited(3)), vm],
            outputs: vec![OutputValue::new("url".into(), false, Some("http://127.0.0.1:41001".into()))],
            native: None,
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "state": "ready", "live": true, "desired": "running", "provider": "virtualbox",
                "machines": [
                    {"name": "web", "state": "running", "published": [{"port": 80, "protocol": "tcp", "host_port": 41001}]},
                    {"name": "job", "state": {"exited": 3}, "published": []},
                    {"name": "vm", "state": {"unknown": "saved"}, "published": [{"port": 22, "protocol": null, "host_port": 2222}], "address": "192.0.2.10"}
                ],
                "outputs": [{"name": "url", "sensitive": false, "value": "http://127.0.0.1:41001"}]
            })
        );
        assert_eq!(serde_json::from_value::<Status>(json).unwrap(), status);
    }

    #[test]
    fn a_status_from_before_live_reads_as_live() {
        let json = r#"{"state":"ready","provider":null,"machines":[],"outputs":[]}"#;
        let status: Status = serde_json::from_str(json).unwrap();
        assert!(status.live);
    }

    #[test]
    fn a_sensitive_output_is_masked_and_kept() {
        let out = OutputValue::new("token".into(), true, Some("s3cret".into()));
        assert_eq!((out.value.as_deref(), out.revealed()), (Some(MASK), Some("s3cret")));
        assert!(!serde_json::to_string(&out).unwrap().contains("s3cret"));
    }
}
