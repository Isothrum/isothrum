//! Running a lab on this machine, and capturing its traffic.
//!
//! A lab is the `isoloom.yml` the editor writes. Running it: Isoloom's library generates the
//! Docker Compose files into the lab's folder, and `docker compose` brings them up. Capturing:
//! a throwaway container joins the network namespace of one machine on the LAN or cable, finds
//! the interface on that network, and streams `tcpdump` to Wireshark (or to a `.pcap` file):
//! GNS3's "capture on a link", for a lab of containers.
//!
//! A lab with VMs runs through Isoloom's Vagrant output, or its hybrid one when it mixes VMs and
//! containers: `vms.rs`. The lab's folder records which (`runtime`).

mod capture;
mod display;
mod lifecycle;
mod links;
mod live;
mod machines;
mod snapshots;

use std::path::{Path, PathBuf};
use std::process::Command;

use isoloom_spec::model::{Spec, Target};
use isoloom_targets::Lab;

pub(crate) use capture::{CAPTURE_IMAGE, Capture, live_capture_possible, plan_capture, start_capture};
pub(crate) use display::open_display;
pub(crate) use lifecycle::{down, prepare, status, up};
pub(crate) use links::{Link, editor_links, link_action, suspended_links};
pub(crate) use live::{LabLive, lab_live};
pub(crate) use machines::{SHELL, console_command, container, machine_action};
pub(crate) use snapshots::{Snapshot, delete_snapshot, snapshots, take_snapshot, use_snapshot};

#[derive(Debug)]
pub(crate) enum RunError {
    /// The YAML isn't an Isoloom spec.
    Spec(String),
    /// The spec can run, but not as containers on this machine (it has VMs, for example).
    NotDocker(String),
    Io(String),
    /// A command failed; its output says why.
    Command {
        what: String,
        output: String,
    },
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spec(m) | RunError::NotDocker(m) | RunError::Io(m) => write!(f, "{m}"),
            RunError::Command { what, output } => write!(f, "{what} failed:\n{output}"),
        }
    }
}

impl std::error::Error for RunError {}

/// How a lab runs: Docker Compose, or Vagrant (only VMs, or VMs and a Docker host VM for the
/// containers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runtime {
    Docker,
    Vagrant,
    Hybrid,
}

impl Runtime {
    /// The Isoloom target whose files it runs.
    pub(crate) fn target(self) -> Target {
        match self {
            Runtime::Docker => Target::Docker,
            Runtime::Vagrant => Target::Vagrant,
            Runtime::Hybrid => Target::Hybrid,
        }
    }

    /// How the lab's folder records it.
    fn name(self) -> &'static str {
        match self {
            Runtime::Docker => "docker",
            Runtime::Vagrant => "vagrant",
            Runtime::Hybrid => "hybrid",
        }
    }
}

/// Where the lab's folder records its runtime.
const RUNTIME: &str = "runtime";

/// The runtime the lab was prepared for (Docker when nothing says otherwise).
pub(crate) fn runtime(dir: &Path) -> Runtime {
    match std::fs::read_to_string(dir.join(RUNTIME))
        .unwrap_or_default()
        .trim()
    {
        "vagrant" => Runtime::Vagrant,
        "hybrid" => Runtime::Hybrid,
        _ => Runtime::Docker,
    }
}

/// Whether the lab has been prepared (its generated files are there).
pub(crate) fn prepared(dir: &Path) -> bool {
    match runtime(dir) {
        Runtime::Docker => dir.join(".isoloom/docker/compose.yml").is_file(),
        rt => crate::vms::dir(dir, rt).join("Vagrantfile").is_file(),
    }
}

/// A lab's folder under the app's data directory: `<data>/labs/<name>/`.
pub(crate) fn lab_dir(data: &Path, name: &str) -> PathBuf {
    data.join("labs").join(name)
}

/// `docker compose` on the lab's file, from its folder.
pub(crate) fn compose(dir: &Path, args: &[&str]) -> Command {
    let mut c = Command::new("docker");
    c.current_dir(dir)
        .args(["compose", "-f", ".isoloom/docker/compose.yml"]);
    // Machines brought back from a snapshot run its image (see `restore_snapshot`).
    if dir.join(snapshots::SNAPSHOT_OVERRIDE).is_file() {
        c.args(["-f", snapshots::SNAPSHOT_OVERRIDE]);
    }
    c.args(args);
    c
}

/// Runs a command to the end; its output becomes the error when it fails.
pub(crate) fn run(mut cmd: Command, what: &str) -> Result<String, RunError> {
    let out = cmd.output().map_err(|e| RunError::Command {
        what: what.into(),
        output: e.to_string(),
    })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        Err(RunError::Command {
            what: what.into(),
            output: text,
        })
    }
}

/// The spec's network that the editor names `name`. Isoloom flattens a switch's VLANs into
/// networks of their own (`sw1.vlan10` becomes `sw1-vlan10`), while the editor names a VLAN's
/// LAN `vlan10` (a name it gives one switch only); a plain LAN or cable keeps its name.
pub(crate) fn spec_network(spec: &Spec, name: &str) -> Result<String, RunError> {
    if spec.networks.contains_key(name) {
        return Ok(name.to_owned());
    }
    let (lan, vlan) = match name.split_once('.') {
        Some((lan, vlan)) => (Some(lan), vlan),
        None => (None, name),
    };
    let id: Option<u16> = vlan.strip_prefix("vlan").and_then(|v| v.parse().ok());
    spec.networks
        .iter()
        .find(|(_, n)| {
            n.vlan
                .as_ref()
                .is_some_and(|(l, i)| Some(*i) == id && lan.is_none_or(|lan| lan == l))
        })
        .map(|(flat, _)| flat.clone())
        .ok_or_else(|| RunError::Spec(format!("the lab has no network `{name}`")))
}

/// The editor's name for one of the spec's networks: `vlan10` for a VLAN's, else its own.
pub(crate) fn editor_network(spec: &Spec, network: &str) -> String {
    match spec.networks.get(network).and_then(|n| n.vlan.as_ref()) {
        Some((_, id)) => format!("vlan{id}"),
        None => network.to_owned(),
    }
}

/// The lab in `yaml`, Isoloom's format: the parser's problems when it can't read it.
pub(crate) fn lab_of(yaml: &str) -> Result<Lab, RunError> {
    Lab::parse(yaml).map_err(|problems| RunError::Spec(lines(&problems)))
}

/// Problems as Isoloom words them, one after the other.
pub(crate) fn lines(problems: &[isoloom_spec::syntax::Problem]) -> String {
    let list: Vec<String> = problems.iter().map(ToString::to_string).collect();
    list.join("\n")
}

/// The spec in `yaml` as `runtime` runs it: lowered for its target (each machine in the form that
/// target gives it, a switch's VLANs as networks of their own).
pub(crate) fn spec_for(yaml: &str, runtime: Runtime) -> Result<Spec, RunError> {
    Ok(lab_of(yaml)?.for_target(runtime.target()))
}

/// The lab's spec as last started (its folder's isoloom.yml), as its runtime runs it.
pub(crate) fn lab_spec(dir: &Path) -> Result<Spec, RunError> {
    let yaml = std::fs::read_to_string(dir.join("isoloom.yml"))
        .map_err(|_| RunError::Spec("start the lab first".into()))?;
    spec_for(&yaml, runtime(dir))
}
