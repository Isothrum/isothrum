//! The `terraform` driver: a module Isoloom generated (Proxmox, the clouds), its state kept by the
//! runner in the run's state folder, never beside the module, so a reinstalled run never orphans
//! what it created.
//!
//! - Its actions: `terraform.clear_lock` (a crashed run's lock, safe to remove since the run lock
//!   makes the runner the state's only writer), `terraform.read_state` (the status: the state
//!   file, no process) and `terraform.copy_module` (a run's own module copied into the run's
//!   state folder, [`copy_module`]). `ssh.run` is the executor's. A module the run copies is
//!   checked where it is, before anything runs.
//! - Its status: every machine of the manifest is running when the state holds resources none of
//!   which is tainted, unhealthy when one is, absent with none; then each VM its server can be
//!   asked about (a Proxmox VE VM with Proxmox's console, through its API) as the server says it
//!   stands: removed or powered off outside the run ([`on_server`]). The published ports come from the
//!   module's `published` output (`"<machine>/<port>" = "<address>:<host port>"`), the other
//!   outputs from the state's.
//! - Every process gets the shared plugin cache (one copy of each provider for every run), and
//!   with an `expiry` the computed expiry as its Terraform variable.
//! - A `down` whose destroy failed on resources already gone, as the manifest's
//!   `identity.terraform.gone` says, drops them from the state and runs again ([`gone`]).
//! - After a successful `down`: the state, its data folder and the module's copy go (the run's
//!   SSH files and the project's archive go for every driver).
//! - No on-demand access machine: the module makes every machine it has at `up`.

pub(crate) mod argv;
mod console;
mod copy_module;
pub(crate) mod gone;
mod guest;
pub(crate) mod state;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Action, ExpiryUnit, Machine, Step, TerraformIdentity};

use super::{Driver, GuestUse, Observed};
use crate::console::ShellCommand;
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::status::{MachineState, MachineStatus, PublishedPort};
use crate::vars::PortKey;
use crate::{Error, Result, RunnerConfig};

/// The local backend's lock file, left behind by a crashed or killed run.
const LOCK_FILE: &str = ".terraform.tfstate.lock.info";

/// What a successful `down` leaves no use for, in the run's state folder.
const FORGOTTEN: [&str; 4] = [state::STATE_FILE, state::BACKUP_FILE, ".terraform", copy_module::COPY];

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

    /// What a failed `down` may print for each instance of the state that is already gone: the
    /// manifest's `identity.terraform.gone`, filled for each instance of its types the state holds.
    pub(crate) fn gone(&self, cx: &Cx<'_>) -> Result<Vec<gone::Gone>> {
        if self.identity.gone.is_empty() {
            return Ok(Vec::new());
        }
        let dir = cx.state_dir();
        Ok(state::read_json(dir)?.map(|s| gone::in_state(&s, &self.identity.gone)).unwrap_or_default())
    }

    /// Drops from the state each instance it still holds whose text, among `seen`, its failed
    /// destroy `step` of list `list` printed ([`gone::forget`]); whether it dropped any.
    pub(crate) async fn forget_gone(&self, cx: &Cx<'_>, list: &str, step: &Step, seen: &std::collections::BTreeSet<String>) -> Result<bool> {
        gone::forget(cx, list, step, self.gone(cx)?, seen).await
    }

    /// The folder whose module the run applies, checked before anything runs: the run's own
    /// module where it is when `up` copies one (`terraform.copy_module`), else the generated one.
    fn checked_module(&self, cx: &Cx<'_>) -> Result<PathBuf> {
        match copied_from(cx)? {
            Some((at, from)) => copy_module::inside_project(cx.run.dir(), &at, &from),
            None => Ok(self.module(cx)),
        }
    }
}

/// The plugin cache the runs use: the user's own (`TF_PLUGIN_CACHE_DIR`), else the one every run
/// of the runner shares, under its home.
pub(crate) fn plugin_cache(config: &RunnerConfig) -> PathBuf {
    config.tf_plugin_cache.clone().unwrap_or_else(|| config.home.join("terraform-plugins"))
}

/// `terraform.copy_module`, its `from` and `when` read from step `step` of list `list`.
fn copy_module(cx: &Cx<'_>, list: &str, step: &Step) -> Result<()> {
    let (at, with) = (crate::executor::plan::at(list, step), super::with_of(step));
    let (from, when) = (cx.vars.with_arg(with, &at, "from")?, cx.vars.with_arg(with, &at, "when")?);
    copy_module::run(cx, &at, from.as_deref(), when.as_deref())
}

impl Driver for Terraform {
    fn preflight(&self, cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        let checked = self.checked_module(cx).and_then(|module| {
            if copy_module::has_terraform_files(&module)? {
                return Ok(());
            }
            Err(Error::Malformed {
                at: format!("the Terraform module {}", module.display()),
                reason: "it has no `.tf` file: generate the run's files again".to_owned(),
            })
        });
        std::future::ready(checked)
    }

    /// Creates the runner's own plugin cache; the user's is theirs, used as it is.
    fn prepare(&self, cx: &Cx<'_>) -> Result<()> {
        let config = &cx.run.runner.config;
        if config.tf_plugin_cache.is_some() {
            return Ok(());
        }
        let cache = plugin_cache(config);
        crate::store::create_dir(&cache)
    }

    fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        let cache = plugin_cache(&cx.run.runner.config);
        let mut env = vec![("TF_PLUGIN_CACHE_DIR".to_owned(), cache.display().to_string())];
        env.extend(expiry_var(cx));
        Ok(env)
    }

    fn action(&self, cx: &Cx<'_>, list: &str, step: &Step, action: Action) -> impl Future<Output = Result<()>> {
        let state_dir = cx.state_dir();
        let done = match action {
            Action::TerraformClearLock => remove(&state_dir.join(LOCK_FILE)),
            Action::TerraformReadState => state::read(state_dir).map(drop),
            Action::TerraformCopyModule => copy_module(cx, list, step),
            other => Err(super::foreign_action(isoloom_manifest::Driver::Terraform, step, other)),
        };
        std::future::ready(done)
    }

    /// The run as its state says it is, then, for a run with resources, each VM as its server
    /// says it stands ([`on_server`]).
    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let tf = state::read(cx.state_dir())?;
        let mut seen = observed(cx.run.manifest(), &tf);
        if tf.resources {
            on_server(cx, &mut seen).await?;
        }
        Ok(seen)
    }

    /// Nothing: a module's resources are the state's, which its destroy reads.
    fn before_down(&self, _cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        std::future::ready(Ok(()))
    }

    fn after_down(&self, cx: &Cx<'_>) -> impl Future<Output = Result<()>> {
        let state_dir = cx.state_dir();
        std::future::ready(FORGOTTEN.iter().try_for_each(|f| remove(&state_dir.join(f))))
    }

    /// Nothing: a module's machine carries no boot digest; the update's apply changes it.
    fn restart(&self, _cx: &Cx<'_>, _machine: &Machine, _id: &str) -> impl Future<Output = Result<()>> {
        std::future::ready(Ok(()))
    }

    /// Refused: the module makes every machine it has at `up`, none is brought up later.
    fn attach(&self, _cx: &Cx<'_>, _machine: &Machine, _image: Option<&str>, _network: Option<&str>) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(on_demand("hot-adding")))
    }

    /// Nothing to do for a machine the module makes: the update's apply removes what the module
    /// no longer makes. Refused for an on-demand access machine (as [`Terraform::attach`]) and
    /// for a Compose service with no VM to run in.
    fn retire(&self, _cx: &Cx<'_>, machine: &Machine) -> impl Future<Output = Result<()>> {
        std::future::ready(if machine.on_demand {
            Err(on_demand("taking out"))
        } else if super::is_compose_service(machine) {
            Err(super::no_vm_for("taking out", machine))
        } else {
            Ok(())
        })
    }

    /// The command that opens a console into `machine`, as `user` when given: `ssh -t` to it
    /// ([`console::command`]).
    fn console_command(&self, cx: &Cx<'_>, machine: &Machine, user: Option<&str>) -> Result<ShellCommand> {
        console::command(cx, machine, user)
    }

    /// `ssh` to the run's VM, as the guest's steps reach it ([`console::in_guest`]).
    fn in_guest(&self, cx: &Cx<'_>, _vm: &Machine, script: &str, use_: GuestUse) -> Result<ProcessSpec> {
        console::in_guest(cx, script, use_)
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
    let published = published(tf);
    let machines = manifest
        .machines
        .iter()
        .map(|m| MachineStatus {
            name: m.name.to_string(),
            role: m.role,
            state: machine_state.clone(),
            addresses: super::static_addresses(m),
            published: m
                .ports
                .iter()
                .filter_map(|p| {
                    let host_port = *published.get(&(m.name.to_string(), p.name.to_string()))?;
                    Some(PublishedPort {
                        port: p.port.get(),
                        protocol: p.kind.transport(),
                        host_port,
                    })
                })
                .collect(),
            console: None,
        })
        .collect();
    Observed {
        machines,
        published,
        // The module's outputs give where the run answers (`${outputs.ip}`).
        hosts: BTreeMap::new(),
        outputs: tf.outputs.clone(),
        sensitive: tf.sensitive.clone(),
        busy: None,
    }
}

/// Each machine of `seen` as its server says it stands, where the server can be asked (a
/// Proxmox VE VM, through its API): a VM removed in the server's own UI is absent, one powered
/// off there stopped, whatever the run's state still holds. Terraform's state changes only at the
/// next apply or destroy, so it alone read such a VM as running. A VM the state holds tainted
/// stays unhealthy while it runs.
///
/// # Errors
///
/// `server_auth_failed` when the server refuses the run's credentials.
async fn on_server(cx: &Cx<'_>, seen: &mut Observed) -> Result<()> {
    let mut vars = cx.vars.clone();
    let read = crate::outputs::read(cx.run.manifest(), &vars, &seen.outputs, &seen.sensitive);
    crate::outputs::bind(&mut vars, &read.values);
    let states = crate::graphical::proxmox_vm_states(cx, &vars).await?;
    for m in &mut seen.machines {
        match states.get(&m.name) {
            Some(MachineState::Running) | None => {}
            Some(state) => m.state = state.clone(),
        }
    }
    Ok(())
}

/// The host port of each published machine port, as the state's `published` output gives them
/// (`"<machine>/<port>": "<address>:<port>"`); an entry that doesn't read so is left out, and
/// traced.
fn published(tf: &state::TfState) -> BTreeMap<PortKey, u16> {
    let mut out = BTreeMap::new();
    for (key, address) in tf.maps.get(PUBLISHED).into_iter().flatten() {
        if let (Some((machine, port)), Some(host_port)) = (key.split_once('/'), state::port_of(address)) {
            out.insert((machine.to_owned(), port.to_owned()), host_port);
        } else {
            tracing::warn!(
                key,
                address,
                "a `published` entry isn't `<machine>/<port>` at `<address>:<port>`: it is left out"
            );
        }
    }
    out
}

/// The first `terraform.copy_module` of `up` that names its folder: where its step is
/// (`lifecycle.up.<id>`) and that folder (`with.from`), filled.
fn copied_from(cx: &Cx<'_>) -> Result<Option<(String, String)>> {
    for step in &cx.run.manifest().lifecycle.up {
        if let isoloom_manifest::Run::Action {
            action: Action::TerraformCopyModule,
            with,
        } = &step.run
            && let at = crate::executor::plan::at(crate::executor::UP, step)
            && let Some(from) = cx.vars.with_arg(with, &at, "from")?
        {
            return Ok(Some((at, from)));
        }
    }
    Ok(None)
}

/// The manifest's expiry variable, as the module takes it, when the run has an expiry.
fn expiry_var(cx: &Cx<'_>) -> Option<(String, String)> {
    let expiry = cx.run.manifest().expiry.as_ref()?;
    let at = cx.expires_at?;
    let value = match expiry.unit {
        ExpiryUnit::UnixSeconds => at.to_string(),
    };
    Some((format!("TF_VAR_{}", expiry.tf_var), value))
}

/// Removes `path` (a file or a folder); nothing there is fine.
fn remove(path: &Path) -> Result<()> {
    crate::store::remove(path)
}

#[cfg(test)]
mod tests;
