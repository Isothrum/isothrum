//! What a run looks like from outside: its state, machines, networks and outputs. Roles,
//! addresses and networks come from the manifest; the engine only says what runs and which host
//! ports it got.

use std::collections::BTreeMap;
use std::path::PathBuf;

use isoloom_manifest::{Manifest, OutputRole, Protocol, Role};
use serde::{Deserialize, Serialize};

use crate::state::RunState;
use crate::store::RunRecord;

/// The command that opens an interactive shell into a machine: the runner builds it (it alone
/// knows the runtime — the Compose project and files, the Vagrant define), the embedder runs it
/// in its own terminal. Addresses the machine by service or define name, never a guessed
/// container.
#[derive(Debug, Clone, Serialize)]
pub struct ShellCommand {
    /// The program to run (`docker`, `vagrant`).
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// The folder to run it in (the run's folder).
    pub cwd: PathBuf,
    /// Variables to run it with, besides the embedder's own (a Vagrant run's state folder,
    /// `VAGRANT_DOTFILE_PATH`, without which Vagrant doesn't find the VM; its forwarded ports).
    /// Empty for a Compose run.
    #[serde(default)]
    pub env: Vec<(String, String)>,
}

impl ShellCommand {
    /// The command as one line a POSIX shell runs as it is, to paste in a terminal:
    /// `cd <cwd> && VAR=value… program args…`, each word that needs it single-quoted (a `sh -c`
    /// script with `;` or `>`, a JSON value), so the shell splits it back into this argv.
    #[must_use]
    pub fn sh_line(&self) -> String {
        let cwd = sh_word(&self.cwd.display().to_string());
        let env = self.env.iter().map(|(k, v)| format!("{k}={}", sh_word(v)));
        let argv = std::iter::once(&self.program).chain(&self.args).map(|w| sh_word(w));
        let words: Vec<String> = env.chain(argv).collect();
        format!("cd {cwd} && {}", words.join(" "))
    }
}

/// `word` as a POSIX shell reads it back: as it is when nothing in it is special, else
/// single-quoted.
fn sh_word(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-.,/:@%+=".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        word.to_owned()
    } else {
        crate::ssh::sh_quote(word)
    }
}

/// A machine's state, as its engine reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NetworkView {
    /// Its name in the spec.
    pub name: String,
    /// Its block.
    pub cidr: String,
    /// Whether it reaches the internet.
    pub internet: bool,
    /// Its name where the target created it (`flag-portal_lab`), when known.
    pub native: Option<String>,
}

/// An output's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OutputValue {
    /// Its name.
    pub name: String,
    /// What it is for (`entry` is the Open button).
    pub role: OutputRole,
    /// Whether the value is sensitive.
    pub sensitive: bool,
    /// Its value, when it can be read.
    pub value: Option<String>,
}

/// A run as it stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    /// The line a shell is given splits back into the command: its folder, its variables, and
    /// each argument whole, whatever it holds.
    #[cfg(unix)]
    #[test]
    fn a_shell_line_splits_back_into_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("a run's folder");
        std::fs::create_dir(&cwd).unwrap();
        let args = [
            "-c",
            r#"printf '%s\n' "$(pwd -P)" "$J" "$@""#,
            "argv0",
            "a b",
            "x; echo no > /dev/null",
            "it's",
            "",
            "$HOME",
        ];
        let cmd = ShellCommand {
            program: "sh".into(),
            args: args.map(str::to_owned).to_vec(),
            cwd: cwd.clone(),
            env: vec![("J".into(), r#"{"a": [1, "b c"]}"#.into())],
        };
        let line = cmd.sh_line();
        assert!(line.starts_with("cd '"), "{line}");
        let out = std::process::Command::new("sh").args(["-c", &line]).output().unwrap();
        let said = String::from_utf8(out.stdout).unwrap();
        let pwd = cwd.canonicalize().unwrap().display().to_string();
        let expected = [pwd.as_str(), r#"{"a": [1, "b c"]}"#, "a b", "x; echo no > /dev/null", "it's", "", "$HOME"];
        assert_eq!(said.lines().collect::<Vec<_>>(), expected, "{line}");
    }

    #[test]
    fn a_plain_word_stays_as_it_is() {
        let cmd = ShellCommand {
            program: "vagrant".into(),
            args: vec!["ssh".into(), "web".into()],
            cwd: PathBuf::from("/labs/flag-vm"),
            env: vec![("VAGRANT_DOTFILE_PATH".into(), "/home/u/.isothrum/runs/flag-vm/vagrant".into())],
        };
        assert_eq!(
            cmd.sh_line(),
            "cd /labs/flag-vm && VAGRANT_DOTFILE_PATH=/home/u/.isothrum/runs/flag-vm/vagrant vagrant ssh web"
        );
    }

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
