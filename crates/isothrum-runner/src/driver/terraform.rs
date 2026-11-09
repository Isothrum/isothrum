//! The `terraform` driver: a module Isoloom generated (Proxmox, the clouds), its state kept by the
//! runner in the run's state folder, never beside the module, so a reinstalled run never orphans
//! what it created. Ported from the launcher's `runtime/terraform/*`.
//!
//! - Its actions: `terraform.clear_lock` (a crashed run's lock, safe to remove since the run lock
//!   makes the runner the state's only writer), `terraform.read_state` (the status: the state
//!   file, no process) and `terraform.copy_module` (a run's own module copied into the run's
//!   state folder, [`copy_module`]). `ssh.run` is the executor's. A module the run copies is
//!   checked where it is, before anything runs.
//! - Its status: every machine of the manifest is running when the state holds resources none of
//!   which is tainted, unhealthy when one is, absent with none. The published ports come from the
//!   module's `published` output (`"<machine>/<port>" = "<address>:<host port>"`), the other
//!   outputs from the state's.
//! - Every process gets the shared plugin cache (one copy of each provider for every run), and
//!   with an `expiry` the computed expiry as its Terraform variable.
//! - After a successful `down`: the state, its data folder, the run's `known_hosts` and SSH
//!   configuration, the project's archive and the module's copy go.
//! - No on-demand access machine: the module makes every machine it has at `up`.

mod console;
mod copy_module;
mod guest;
mod state;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Action, ExpiryUnit, Machine, SnakeName, Step, Template, TerraformIdentity};

use super::{Driver, Observed};
use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::status::{MachineState, MachineStatus, ShellCommand};

/// The local backend's lock file, left behind by a crashed or killed run.
const LOCK_FILE: &str = ".terraform.tfstate.lock.info";

/// What a successful `down` leaves no use for, in the run's state folder.
const FORGOTTEN: [&str; 6] = [
    state::STATE_FILE,
    "terraform.tfstate.backup",
    ".terraform",
    "known_hosts",
    "ssh_config",
    copy_module::COPY,
];

/// The output holding the published ports.
const PUBLISHED: &str = "published";

/// The `terraform` driver of one manifest.
#[derive(Debug, Clone)]
pub(crate) struct Terraform {
    identity: TerraformIdentity,
}

impl Terraform {
    pub(crate) fn new(identity: TerraformIdentity) -> Terraform {
        Terraform { identity }
    }

    /// The module's folder.
    fn module(&self, cx: &Cx<'_>) -> PathBuf {
        cx.run.dir().join(self.identity.module.as_str())
    }
}

/// The plugin cache every run of a runner shares, under its home.
pub(crate) fn plugin_cache(home: &Path) -> PathBuf {
    home.join("terraform-plugins")
}

impl Driver for Terraform {
    fn preflight(&self, cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        let module = self.module(cx);
        // A run's own module the run copies (`terraform.copy_module`) is checked where it is.
        let copied = copied_from(cx).and_then(|from| copy_module::inside_project(cx.run.dir(), &from));
        let has_tf = copy_module::has_terraform_files(copied.as_deref().unwrap_or(&module));
        let missing = std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no Terraform module (no `.tf` file): generate the run's files again",
        );
        std::future::ready(if has_tf { Ok(()) } else { Err(Error::io(&module, "read")(missing)) })
    }

    fn prepare(&self, cx: &Cx<'_>) -> Result<()> {
        let cache = plugin_cache(&cx.run.runner.config.home);
        crate::store::private_dir(&cache).map_err(Error::io(&cache, "created"))
    }

    fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        let cache = plugin_cache(&cx.run.runner.config.home);
        let mut env = vec![("TF_PLUGIN_CACHE_DIR".to_owned(), cache.display().to_string())];
        env.extend(expiry_var(cx));
        Ok(env)
    }

    fn action(&self, cx: &Cx<'_>, step: &Step, action: Action, with: &BTreeMap<SnakeName, Template>) -> impl Future<Output = Result<()>> {
        let done = cx.run.runner.store.run_dir(cx.run.name()).and_then(|state_dir| match action {
            Action::TerraformClearLock => remove(&state_dir.join(LOCK_FILE)),
            Action::TerraformReadState => state::read(&state_dir).map(drop),
            Action::TerraformCopyModule => {
                let at = step.id.as_str();
                let (from, when) = (cx.vars.with_arg(with, at, "from")?, cx.vars.with_arg(with, at, "when")?);
                copy_module::run(cx, at, from.as_deref(), when.as_deref())
            }
            other => Err(super::foreign_action("Terraform", step, other)),
        });
        std::future::ready(done)
    }

    fn observe(&self, cx: &Cx<'_>) -> impl Future<Output = Result<Observed>> {
        let read = cx.run.runner.store.run_dir(cx.run.name()).and_then(|dir| state::read(&dir));
        std::future::ready(read.map(|tf| observed(cx.run.manifest(), &tf)))
    }

    fn after_down(&self, cx: &Cx<'_>) -> Result<()> {
        let state_dir = cx.run.runner.store.run_dir(cx.run.name())?;
        FORGOTTEN.iter().try_for_each(|f| remove(&state_dir.join(f)))
    }

    /// Refused: the module makes every machine it has at `up`, none is brought up later.
    fn attach(&self, _cx: &Cx<'_>, _native: &str, _image: Option<&str>, _network: Option<&str>) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(on_demand("hot-adding")))
    }

    /// Refused, as [`Terraform::attach`].
    fn detach(&self, _cx: &Cx<'_>, _native: &str) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(on_demand("hot-removing")))
    }

    /// A machine the module makes is left to the update's apply; a Compose service in the run's
    /// VM is removed there, with its namespace ([`console::retire`]).
    async fn retire(&self, cx: &Cx<'_>, machine: &Machine) -> Result<()> {
        console::retire(cx, machine).await
    }

    /// The command that opens a console into `machine`: `ssh -t` to it ([`console::command`]).
    fn console_command(&self, cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand> {
        console::command(cx, machine)
    }
}

/// What a manifest's on-demand access machine on a Terraform run is told: the module makes every
/// machine it has at `up`, none is brought up later.
fn on_demand(what: &str) -> Error {
    Error::Unsupported {
        what: format!("{what} an on-demand access machine on a server or cloud target"),
    }
}

/// The run as `tf` says it is: each machine of `manifest`, its published ports, the outputs.
fn observed(manifest: &isoloom_manifest::Manifest, tf: &state::TfState) -> Observed {
    let machine_state = match (tf.resources, tf.tainted) {
        (false, _) => MachineState::Absent,
        (true, true) => MachineState::Unhealthy,
        (true, false) => MachineState::Running,
    };
    let machines = manifest
        .machines
        .iter()
        .map(|m| MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state: machine_state.clone(),
            addresses: super::static_addresses(m),
            published: Vec::new(),
        })
        .collect();
    let published = tf
        .maps
        .get(PUBLISHED)
        .into_iter()
        .flatten()
        .filter_map(|(key, address)| {
            let (machine, port) = key.split_once('/')?;
            Some(((machine.to_owned(), port.to_owned()), state::port_of(address)?))
        })
        .collect();
    Observed {
        machines,
        published,
        // The module's outputs give where the run answers (`${outputs.ip}`).
        hosts: BTreeMap::new(),
        outputs: tf.outputs.clone(),
        sensitive: tf.sensitive.clone(),
    }
}

/// The folder the first `terraform.copy_module` of `up` copies (`with.from`), filled.
fn copied_from(cx: &Cx<'_>) -> Option<String> {
    let up = cx.run.manifest().lifecycle.up.as_slice();
    up.iter().find_map(|s| match &s.run {
        isoloom_manifest::Run::Action {
            action: Action::TerraformCopyModule,
            with,
        } => cx.vars.with_arg(with, s.id.as_str(), "from").ok().flatten(),
        _ => None,
    })
}

/// The manifest's expiry variable, as the module takes it, when the run has an expiry.
fn expiry_var(cx: &Cx<'_>) -> Option<(String, String)> {
    let expiry = cx.run.manifest().expiry.as_ref()?;
    let at = cx.expires_at?;
    let value = match expiry.unit {
        ExpiryUnit::UnixSeconds => at.to_string(),
        ExpiryUnit::MinutesFromNow => {
            let now = crate::clock::unix_seconds(cx.run.runner.config.clock.now());
            (at.saturating_sub(now) / 60).to_string()
        }
    };
    Some((format!("TF_VAR_{}", expiry.tf_var), value))
}

/// Removes `path` (a file or a folder); nothing there is fine.
fn remove(path: &Path) -> Result<()> {
    let done = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match done {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::io(path, "removed")(e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
