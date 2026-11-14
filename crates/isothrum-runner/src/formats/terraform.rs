//! The Terraform driver: any root module, run with a lifecycle fixed for the format, through the
//! Terraform adapter ([`crate::cli::terraform`]) only.
//!
//! - Its state is the run's own: Terraform's data folder (`TF_DATA_DIR`) and the local backend's
//!   state file are in the run's state folder, never beside the module, so two copies of one
//!   module never share a state and a module folder reinstalled never orphans what a run made. A
//!   module whose own backend isn't local keeps it: every copy then shares that backend's state,
//!   and the run is warned.
//! - `up` and `update`: `init` (with the plugin cache the core gives in `TF_PLUGIN_CACHE_DIR`),
//!   then `apply -auto-approve`. The run is ready when the apply ends well: a module that sets
//!   machines up waits for them within its own apply. A lock another run's crash left on a
//!   backend's state is released once (`force-unlock`), and the call made again; a local state's
//!   lock is the operating system's, gone with the process that held it.
//! - `down`: `destroy -auto-approve`. A destroy that fails because a provider says an object of
//!   the state is already gone has those instances dropped from the state (`state rm`, each
//!   warned), then runs again, once. Then the run's own state files go.
//! - The resources are the state's managed instances (`show -json`), by address and id, so what
//!   a crashed run made is removed by its recorded id ([`FormatDriver::destroy_by_id`]: a
//!   targeted destroy, after an `import` when the state lost it).
//! - The machines are the module's VMs ([`machines`]); `observe` reads them and the outputs from
//!   the state (`show -json`, `output -json`, sensitivity kept).
//! - Inputs reach the module as `TF_VAR_<name>` in its environment only: never on a command line,
//!   never in a file (no `-var-file`, no saved plan, which would hold their values), and their
//!   values are masked in the lines and errors the driver hands on.
//! - Parking, resuming, provisioning, attaching, retiring, running a command and a console are
//!   refused: a module's machines are made and removed by its apply and destroy only.
//! - A Proxmox VE VM's screen is reached through the server's API ([`screen`]); a Windows VM's
//!   Remote Desktop (or a VNC server the module names) through an SSH tunnel to the Linux
//!   machine beside it, as the module's outputs say ([`tunnelled`]).

mod machines;
mod screen;
mod tunnelled;

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{Change, FormatDriver, Found, Output, Ran, Seen, SeenMachine, ToolCx, tool_error};
use crate::Result;
use crate::cli::terraform::{self as tf, Backend, Failure, StateAt};
use crate::cli::{Call, CliError};
use crate::console::ShellCommand;
use crate::error::Error;
use crate::events::{Stream, WarningCode};
use crate::state::{Park, RunState};
use crate::status::MachineState;
use crate::store::{Lifetime, ResourceKind};

/// The local backend's state file, in the run's state folder.
const STATE_FILE: &str = "terraform.tfstate";

/// The state Terraform kept before its last write, beside [`STATE_FILE`].
const BACKUP_FILE: &str = "terraform.tfstate.backup";

/// What the local backend writes of its lock beside the state: what a killed run leaves (the lock
/// itself is the operating system's, released with the process).
const LOCK_INFO: &str = ".terraform.tfstate.lock.info";

/// Terraform's data folder (providers, modules, the backend `init` chose), in the run's state
/// folder.
const DATA_DIR: &str = ".terraform";

/// Where Terraform reads its data folder from.
const DATA_DIR_VAR: &str = "TF_DATA_DIR";

/// Tells Terraform no person reads it: no "run terraform plan next" hints.
const AUTOMATION_VAR: &str = "TF_IN_AUTOMATION";

/// The prefix of a module variable's value in the environment: the inputs.
const INPUT_PREFIX: &str = "TF_VAR_";

/// What a masked input reads as.
const MASK: &str = "***";

/// The shortest input value masked: a shorter one (`1`, `on`, `web`) is no credential, and masking
/// it would hide the words of every line it appears in.
const MASKED_FROM: usize = 4;

/// How long `init` may take: it downloads the module's providers and modules.
const INIT_LIMIT: Duration = Duration::from_secs(30 * 60);

/// How long an `apply`, a `destroy` or a `plan` may take: a cloud's machines, and the waits a module
/// makes for their set-up, run within it.
const APPLY_LIMIT: Duration = Duration::from_secs(2 * 60 * 60);

/// How long a call that only reads or edits the state may take (`show`, `output`, `state rm`,
/// `import`, `force-unlock`, `version`).
const READ_LIMIT: Duration = Duration::from_secs(5 * 60);

/// The Terraform driver.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Terraform;

/// One operation's view of the run: where its state is, and the run's inputs to mask.
struct Module<'c, 'a> {
    cx: &'c ToolCx<'a>,
    data_dir: PathBuf,
    state_file: PathBuf,
    masks: Vec<String>,
}

impl<'c, 'a> Module<'c, 'a> {
    fn of(cx: &'c ToolCx<'a>) -> Module<'c, 'a> {
        let mut masks: Vec<String> = cx
            .env
            .iter()
            .filter(|(name, value)| name.starts_with(INPUT_PREFIX) && value.chars().count() >= MASKED_FROM)
            .map(|(_, value)| value.clone())
            .collect();
        // Longest first, so a value holding another is masked whole.
        masks.sort_by_key(|v| std::cmp::Reverse(v.len()));
        masks.dedup();
        Module {
            cx,
            data_dir: cx.state_dir.join(DATA_DIR),
            state_file: cx.state_dir.join(STATE_FILE),
            masks,
        }
    }

    /// `text` with every input's value masked.
    fn mask(&self, text: &str) -> String {
        self.masks.iter().fold(text.to_owned(), |t, v| t.replace(v.as_str(), MASK))
    }

    /// The line `line` on `stream`, masked, to the operation's step output.
    fn say(&self, stream: Stream, line: &str) {
        (self.cx.sink)(stream, &self.mask(line));
    }

    /// A call of Terraform in the module's folder within `limit`, with the run's environment,
    /// the run's data folder, and `sink` for its lines.
    fn call<'s>(&'s self, limit: Duration, sink: &'s (dyn Fn(Stream, &str) + Send + Sync)) -> Call<'s> {
        let mut call = self.cx.call(limit);
        call.env.push((DATA_DIR_VAR.to_owned(), self.data_dir.display().to_string()));
        call.env.push((AUTOMATION_VAR.to_owned(), "1".to_owned()));
        call.sink = sink;
        call
    }

    /// The runner's error for the adapter's `e` in step `step`, its tool text masked.
    fn error<F: std::fmt::Display>(&self, step: &str, e: CliError<F>) -> Error {
        match tool_error(step, e) {
            Error::ToolFailed { step, reason } => Error::ToolFailed {
                step,
                reason: self.mask(&reason),
            },
            other => other,
        }
    }

    /// Where the state is, as the backend `init` chose says.
    fn state(&self) -> Result<StateAt<'_>> {
        match tf::backend(&self.data_dir) {
            Ok(Backend::Local) => Ok(StateAt::File(&self.state_file)),
            Ok(Backend::Other(_)) => Ok(StateAt::Backend),
            Err(reason) => Err(Error::ToolFailed {
                step: "init".to_owned(),
                reason,
            }),
        }
    }

    /// Readies the module for a call that changes it: the lock information a killed run left
    /// removed, `init`, and where its state is (a backend shared by every copy warned).
    async fn init(&self) -> Result<StateAt<'_>> {
        crate::store::remove(&self.cx.state_dir.join(LOCK_INFO))?;
        let say = |s: Stream, l: &str| self.say(s, l);
        tf::init(&self.call(INIT_LIMIT, &say)).await.map_err(|e| self.error("init", e))?;
        if let Backend::Other(name) = tf::backend(&self.data_dir).map_err(|reason| Error::ToolFailed {
            step: "init".to_owned(),
            reason,
        })? {
            (self.cx.warn)(
                WarningCode::SharedState,
                &format!("the module keeps its state in its own `{name}` backend: every copy of it shares that state"),
            );
        }
        self.state()
    }

    /// `change` on the state at `state` as step `step`. A lock left on a backend's state is
    /// released once and the call made again.
    async fn apply(&self, step: &str, state: StateAt<'_>, change: tf::Change<'_>) -> std::result::Result<(), CliError<Failure>> {
        let say = |s: Stream, l: &str| self.say(s, l);
        let call = self.call(APPLY_LIMIT, &say);
        match tf::apply(&call, state, change).await {
            Err(CliError::Failed {
                failure: Failure::StateLocked { id: Some(id) },
                ..
            }) if state == StateAt::Backend => {
                (self.cx.warn)(
                    WarningCode::Retrying,
                    &format!("the state's lock `{id}` is held by no run of this one: released, then `{step}` again"),
                );
                tf::force_unlock(&self.call(READ_LIMIT, &say), &id).await?;
                tf::apply(&call, state, change).await
            }
            done => done,
        }
    }

    /// The state's managed instances.
    async fn shown(&self, state: StateAt<'_>) -> Result<tf::Shown> {
        let say = |s: Stream, l: &str| self.say(s, l);
        tf::show(&self.call(READ_LIMIT, &say), state).await.map_err(|e| self.error("show", e))
    }

    /// `destroy`, with what a provider says is already gone dropped from the state and the
    /// destroy made again, once.
    async fn destroy(&self, state: StateAt<'_>) -> Result<()> {
        let errors = match self.apply("destroy", state, tf::Change::Destroy).await {
            Ok(()) => return Ok(()),
            Err(CliError::Failed {
                failure: Failure::Errors(errors),
                ..
            }) => errors,
            Err(e) => return Err(self.error("destroy", e)),
        };
        let held = self.shown(state).await?;
        let mut gone: Vec<String> = errors
            .iter()
            .filter(|d| d.says_missing())
            .filter_map(|d| d.address.clone())
            .filter(|at| held.managed.iter().any(|r| &r.address == at))
            .collect();
        gone.sort();
        gone.dedup();
        if gone.is_empty() {
            return Err(self.error(
                "destroy",
                CliError::Failed {
                    tool: "terraform",
                    failure: Failure::Errors(errors),
                },
            ));
        }
        for at in &gone {
            let why = errors
                .iter()
                .find(|d| d.address.as_deref() == Some(at))
                .map(|d| d.summary.as_str())
                .unwrap_or_default();
            (self.cx.warn)(
                WarningCode::AlreadyGone,
                &self.mask(&format!("`{at}` is already gone ({why}): dropped from the state, then destroyed again")),
            );
        }
        let say = |s: Stream, l: &str| self.say(s, l);
        tf::state_rm(&self.call(READ_LIMIT, &say), state, &gone)
            .await
            .map_err(|e| self.error("state-rm", e))?;
        self.apply("destroy", state, tf::Change::Destroy).await.map_err(|e| self.error("destroy", e))
    }

    /// The run's own state files, after a destroy that ended well: the local state, its backup and
    /// the data folder. A backend's are the backend's, and its data folder stays (a later read of
    /// that backend needs it).
    fn forget(&self, state: StateAt<'_>) -> Result<()> {
        if state == StateAt::Backend {
            return Ok(());
        }
        [STATE_FILE, BACKUP_FILE, LOCK_INFO, DATA_DIR]
            .iter()
            .try_for_each(|f| crate::store::remove(&self.cx.state_dir.join(f)))
    }
}

/// What Terraform refuses to do with a module's machines.
fn refused(what: &str) -> Error {
    Error::Unsupported {
        what: format!("{what} on a Terraform run: a module's machines are made and removed by its apply and destroy only"),
    }
}

/// An output's value as text: a string as it is, anything else as JSON.
fn text_of(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// The machines among `managed`: each VM by its machine name ([`machines::machine_name`]), with
/// its state (a tainted one is unhealthy: a create or a provisioner of it failed).
fn machines_of(managed: &[tf::Resource]) -> Vec<SeenMachine> {
    managed
        .iter()
        .filter(|r| machines::is_vm(&r.kind))
        .map(|r| SeenMachine {
            name: machines::machine_name(&r.address, &r.kind),
            state: if r.tainted { MachineState::Unhealthy } else { MachineState::Running },
            published: Vec::new(),
            kind: Some(r.kind.clone()),
            address: r.vm.address.clone(),
            windows: r.vm.windows,
        })
        .collect()
}

/// The run's state as its managed instances say: nothing is absent, a tainted one failed, else
/// ready.
fn run_state(managed: &[tf::Resource]) -> RunState {
    if managed.is_empty() {
        RunState::Absent
    } else if managed.iter().any(|r| r.tainted) {
        RunState::Failed
    } else {
        RunState::Ready
    }
}

impl Terraform {
    /// Takes over the deployment whose local state is in `from` (applied outside the runner from
    /// the run's module): its state, and its backup when there is one, move into the run's state
    /// folder, which later operations use as the run's own.
    ///
    /// # Errors
    ///
    /// `adopt` when the run has a state already or `from` holds none; `io` when a file can't be
    /// moved (what moved is put back). The files moved, for [`Terraform::give_back`].
    pub(crate) fn adopt(cx: &ToolCx<'_>, from: &Path) -> Result<Vec<&'static str>> {
        let refuse = |reason: String| Error::Adopt {
            run: cx.run.to_string(),
            reason,
        };
        if cx.state_dir.join(STATE_FILE).exists() {
            return Err(refuse("the run has a Terraform state already".into()));
        }
        if !from.join(STATE_FILE).is_file() {
            return Err(refuse(format!("{} holds no {STATE_FILE}", from.display())));
        }
        let mut moved = Vec::new();
        for file in [STATE_FILE, BACKUP_FILE] {
            let (src, dst) = (from.join(file), cx.state_dir.join(file));
            if !src.exists() {
                continue;
            }
            if let Err(e) = move_file(&src, &dst) {
                Terraform::give_back(cx, from, &moved);
                return Err(Error::io(&src, "moved")(e));
            }
            moved.push(file);
        }
        Ok(moved)
    }

    /// Puts `files` an [`Terraform::adopt`] moved from `from` back (an adoption whose record
    /// couldn't be written), best effort.
    pub(crate) fn give_back(cx: &ToolCx<'_>, from: &Path, files: &[&str]) {
        for file in files {
            if let Err(e) = move_file(&cx.state_dir.join(file), &from.join(file)) {
                tracing::warn!(file, error = %e, "an adopted state file couldn't be put back");
            }
        }
    }
}

/// Moves `from` to `to`: a rename, else, only when they are on different file systems, a copy
/// then a removal. Any other failure to rename is the error.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to)?;
            // Never in both places: when `from` can't be removed, the copy goes too.
            std::fs::remove_file(from).inspect_err(|_| {
                if let Err(gone) = std::fs::remove_file(to) {
                    tracing::warn!(error = %gone, file = %to.display(), "a copy of a taken-over state couldn't be removed");
                }
            })
        }
        renamed => renamed,
    }
}

/// The plugin cache the runs use: the user's own (`TF_PLUGIN_CACHE_DIR`), else the one every run
/// of the runner shares, under its home.
pub(crate) fn plugin_cache(config: &crate::RunnerConfig) -> PathBuf {
    config.tf_plugin_cache.clone().unwrap_or_else(|| config.home.join("terraform-plugins"))
}

impl FormatDriver for Terraform {
    async fn probe(&self, cx: &ToolCx<'_>) -> Result<()> {
        let module = Module::of(cx);
        let say = |s: Stream, l: &str| module.say(s, l);
        tf::version(&module.call(READ_LIMIT, &say))
            .await
            .map(drop)
            .map_err(|e| module.error("probe", e))
    }

    async fn up(&self, cx: &ToolCx<'_>) -> Result<()> {
        let module = Module::of(cx);
        let state = module.init().await?;
        module.apply("apply", state, tf::Change::Apply).await.map_err(|e| module.error("apply", e))
    }

    async fn down(&self, cx: &ToolCx<'_>) -> Result<()> {
        let module = Module::of(cx);
        let state = module.init().await?;
        module.destroy(state).await?;
        module.forget(state)
    }

    fn park(&self, _cx: &ToolCx<'_>, _mode: Park) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(refused("parking")))
    }

    fn resume(&self, _cx: &ToolCx<'_>) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(refused("resuming")))
    }

    /// The same apply as `up`: Terraform brings the module's resources to its files.
    async fn update(&self, cx: &ToolCx<'_>) -> Result<()> {
        self.up(cx).await
    }

    /// The VMs the module's files declare, made or not: those of the state a plan keeps, and
    /// those it would make.
    async fn machines(&self, cx: &ToolCx<'_>) -> Result<Vec<String>> {
        let module = Module::of(cx);
        let state = module.init().await?;
        let say = |s: Stream, l: &str| module.say(s, l);
        let planned = tf::plan(&module.call(APPLY_LIMIT, &say), state).await.map_err(|e| module.error("plan", e))?;
        let held = module.shown(state).await?;
        let removed = |at: &str| planned.iter().any(|p| p.address == at && p.action == tf::Action::Delete);
        let mut names: Vec<String> = held
            .managed
            .iter()
            .filter(|r| machines::is_vm(&r.kind) && !removed(&r.address))
            .map(|r| machines::machine_name(&r.address, &r.kind))
            .chain(
                planned
                    .iter()
                    .filter(|p| machines::is_vm(&p.kind) && matches!(p.action, tf::Action::Create | tf::Action::Replace))
                    .map(|p| machines::machine_name(&p.address, &p.kind)),
            )
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    /// A plan, its changes shown as the operation's output, then the apply. Terraform's apply
    /// reaches the whole module, so `change` only says what moved: the apply plans again (a saved
    /// plan would hold the inputs' values).
    async fn reapply(&self, cx: &ToolCx<'_>, _change: &Change) -> Result<()> {
        let module = Module::of(cx);
        let state = module.init().await?;
        let say = |s: Stream, l: &str| module.say(s, l);
        tf::plan(&module.call(APPLY_LIMIT, &say), state).await.map_err(|e| module.error("plan", e))?;
        module.apply("apply", state, tf::Change::Apply).await.map_err(|e| module.error("apply", e))
    }

    fn provision(&self, _cx: &ToolCx<'_>, _machine: Option<&str>, _with: Option<&str>) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(refused("provisioning a machine again")))
    }

    fn attach(&self, _cx: &ToolCx<'_>, machine: &str) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(refused(&format!("starting `{machine}` on its own"))))
    }

    fn retire(&self, _cx: &ToolCx<'_>, machine: &str) -> impl Future<Output = Result<()>> {
        std::future::ready(Err(refused(&format!("removing `{machine}` on its own"))))
    }

    fn exec(&self, _cx: &ToolCx<'_>, machine: &str, _argv: &[String]) -> impl Future<Output = Result<Ran>> {
        std::future::ready(Err(refused(&format!("running a command in `{machine}`"))))
    }

    fn console(&self, _cx: &ToolCx<'_>, machine: &str, _user: Option<&str>) -> Result<ShellCommand> {
        Err(refused(&format!("opening a console into `{machine}`")))
    }

    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)> {
        let module = Module::of(cx);
        let state = module.state()?;
        let held = module.shown(state).await?;
        let say = |s: Stream, l: &str| module.say(s, l);
        let outputs = match state {
            StateAt::File(file) if !file.exists() => std::collections::BTreeMap::new(),
            _ => tf::outputs(&module.call(READ_LIMIT, &say), state)
                .await
                .map_err(|e| module.error("output", e))?,
        };
        let seen = Seen {
            machines: machines_of(&held.managed),
            outputs: outputs
                .into_iter()
                .map(|(name, o)| {
                    let output = Output {
                        value: text_of(&o.value),
                        sensitive: o.sensitive,
                    };
                    (name, output)
                })
                .collect(),
        };
        Ok((seen, run_state(&held.managed)))
    }

    async fn found(&self, cx: &ToolCx<'_>) -> Result<Vec<Found>> {
        let module = Module::of(cx);
        let held = module.shown(module.state()?).await?;
        Ok(held
            .managed
            .into_iter()
            .filter_map(|r| {
                let kind = if machines::is_vm(&r.kind) { ResourceKind::Vm } else { ResourceKind::Module };
                Some(Found {
                    native_id: r.id?,
                    key: r.address,
                    kind,
                    lifetime: Lifetime::Run,
                })
            })
            .collect())
    }

    /// A destroy of the one instance at `key` when the state holds it with `native_id`. One the
    /// state lost is imported by its id first; there is nothing to remove when no object has that
    /// id any more.
    async fn destroy_by_id(&self, cx: &ToolCx<'_>, key: &str, _kind: ResourceKind, native_id: &str) -> Result<()> {
        let module = Module::of(cx);
        let state = module.init().await?;
        let held = module.shown(state).await?;
        match held.managed.iter().find(|r| r.address == key) {
            Some(r) if r.id.as_deref() == Some(native_id) => {}
            Some(r) => {
                return Err(Error::Unsupported {
                    what: format!(
                        "removing `{key}`'s object `{native_id}`: the state holds another one there (`{}`), and Terraform reaches an object through its address only",
                        r.id.as_deref().unwrap_or("no id")
                    ),
                });
            }
            None => {
                let say = |s: Stream, l: &str| module.say(s, l);
                match tf::import(&module.call(READ_LIMIT, &say), state, key, native_id).await {
                    Ok(()) => {}
                    Err(CliError::Failed {
                        failure: Failure::NoSuchObject,
                        ..
                    }) => return Ok(()),
                    Err(e) => return Err(module.error("import", e)),
                }
            }
        }
        module
            .apply("destroy", state, tf::Change::DestroyOne(key))
            .await
            .map_err(|e| module.error("destroy", e))
    }

    fn graphical(&self, _cx: &ToolCx<'_>, seen: &Seen, machine: &SeenMachine) -> impl Future<Output = Result<Option<crate::graphical::ConsoleOffer>>> {
        std::future::ready(Ok(
            screen::offer(machine).or_else(|| tunnelled::offer(&tunnelled::texts(&seen.outputs), machine))
        ))
    }

    async fn reach_graphical(&self, cx: &ToolCx<'_>, machine: &str) -> Result<crate::graphical::Console> {
        let module = Module::of(cx);
        let state = module.state()?;
        let held = module.shown(state).await?;
        let vm = held
            .managed
            .iter()
            .filter(|r| machines::is_vm(&r.kind))
            .find(|r| machines::machine_name(&r.address, &r.kind) == machine)
            .ok_or_else(|| Error::MachineUnknown {
                run: cx.run.to_string(),
                machine: machine.to_owned(),
            })?;
        if screen::is_proxmox(Some(&vm.kind)) {
            screen::reach(cx, machine).await
        } else {
            tunnelled::reach(&module, state, vm, machine).await
        }
    }

    fn native_app(&self, _cx: &ToolCx<'_>, seen: &Seen) -> Option<crate::graphical::NativeApp> {
        screen::app(seen)
    }

    async fn native_console(&self, cx: &ToolCx<'_>, machine: &str) -> Result<crate::graphical::NativeConsole> {
        screen::native(cx, machine).await
    }
}

#[cfg(test)]
mod tests;
