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
//!   the state (`show -json`, `output -json`, sensitivity kept), each machine at the address its
//!   provider writes, else the one the module's `machines` output names. A resource the provider removed
//!   outside the run stays in the state until a refresh, and a status does none: it is read
//!   often, and a refresh calls the provider's API under the state's lock, with the run's
//!   credentials. The next `up` or `update` refreshes in its apply and makes it again. A Proxmox
//!   VE VM's own state is read from the server ([`screen`]).
//! - Inputs reach the module as `TF_VAR_<name>` in its environment only: never on a command line,
//!   never in a file (no `-var-file`, no saved plan, which would hold their values), and their
//!   values are masked in the lines and errors the driver hands on.
//! - Parking, resuming, provisioning, attaching and retiring are refused: a module's machines are
//!   made and removed by its apply and destroy only.
//! - A console and a command are an `ssh` into the VM, along the route its outputs and the run's
//!   values give ([`route`]: through the module's router, through the run's `ssh_jump` first).
//! - A Proxmox VE VM's screen is reached through the server's API ([`screen`]); a Windows VM's
//!   Remote Desktop (or a VNC server the module names) through an SSH tunnel to the Linux
//!   machine beside it, as the module's outputs say ([`tunnelled`]).

mod adopt;
mod console;
mod jump;
mod machines;
mod required;
mod route;
use machines::{machines_of, run_state, with_named_addresses};
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
use crate::steps::{Phase, StepCount};
use crate::store::{Lifetime, ResourceKind};
use crate::tool_output::sign_in;

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
    /// Variables every call gets beside the run's (the jump host's proxy, [`jump`]).
    extra: Vec<(String, String)>,
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
            extra: Vec::new(),
        }
    }

    /// The same module, every call of it also given `vars`.
    fn with_env(&self, vars: &[(&str, String)]) -> Module<'c, 'a> {
        let mut extra = self.extra.clone();
        extra.extend(vars.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        Module {
            cx: self.cx,
            data_dir: self.data_dir.clone(),
            state_file: self.state_file.clone(),
            masks: self.masks.clone(),
            extra,
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
        call.env.extend(self.extra.iter().cloned());
        call.sink = sink;
        call
    }

    /// The runner's error for the adapter's `e` in step `step`, its tool text masked.
    ///
    /// A failure that says the cloud refused the sign-in (it ran out, or was never made) is
    /// `credentials_expired`, naming the cloud whose provider it names.
    fn error<F: std::fmt::Display>(&self, step: &str, e: CliError<F>) -> Error {
        match tool_error(step, e) {
            Error::ToolFailed { step, reason } if sign_in::expired(&reason) => Error::CredentialsExpired {
                step,
                cloud: sign_in::cloud_named(&reason),
                reason: self.mask(&reason),
            },
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
    /// released once and the call made again. An apply says how far it is: the `apply` step's
    /// count of the resources its plan changes, the `ready` step once a provisioner begins.
    async fn apply(&self, step: &str, state: StateAt<'_>, change: tf::Change<'_>) -> std::result::Result<(), CliError<Failure>> {
        let say = |s: Stream, l: &str| self.say(s, l);
        let call = self.call(APPLY_LIMIT, &say);
        let count = std::sync::Mutex::new(StepCount::new(0, None));
        let steps = &self.cx.steps;
        let applying = |moved: tf::Applying| {
            let mut count = count.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            match moved {
                tf::Applying::Planned(total) => count.total = Some(total),
                tf::Applying::Applied => count.done += 1,
                tf::Applying::Provisioning => {
                    steps.start(Phase::Ready, None);
                    return;
                }
            }
            steps.count(Phase::Apply, None, *count);
        };
        let progress: &(dyn Fn(tf::Applying) + Send + Sync) = if change == tf::Change::Apply { &applying } else { &tf::unheard };
        match tf::apply(&call, state, change, progress).await {
            Err(CliError::Failed {
                failure: Failure::StateLocked { id: Some(id) },
                ..
            }) if state == StateAt::Backend => {
                (self.cx.warn)(
                    WarningCode::Retrying,
                    &format!("the state's lock `{id}` is held by no run of this one: released, then `{step}` again"),
                );
                tf::force_unlock(&self.call(READ_LIMIT, &say), &id).await?;
                tf::apply(&call, state, change, progress).await
            }
            done => done,
        }
    }

    /// The state's managed instances.
    async fn shown(&self, state: StateAt<'_>) -> Result<tf::Shown> {
        tf::show(&self.call(READ_LIMIT, &super::discard), state)
            .await
            .map_err(|e| self.error("show", e))
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
    Error::Declined {
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

/// The VM of `held` whose machine name is `machine`.
///
/// # Errors
///
/// `machine_unknown` when the state has no such VM.
fn vm_named<'h>(cx: &ToolCx<'_>, held: &'h tf::Shown, machine: &str) -> Result<&'h tf::Resource> {
    held.managed
        .iter()
        .filter(|r| machines::is_vm(&r.kind))
        .find(|r| machines::machine_name(&r.address, &r.kind) == machine)
        .ok_or_else(|| Error::MachineUnknown {
            run: cx.run.to_string(),
            machine: machine.to_owned(),
        })
}

/// The plugin cache the runs use: the user's own (`TF_PLUGIN_CACHE_DIR`), else the one every run
/// of the runner shares, under its home.
pub(crate) fn plugin_cache(config: &crate::RunnerConfig) -> PathBuf {
    config.tf_plugin_cache.clone().unwrap_or_else(|| config.home.join("terraform-plugins"))
}

impl FormatDriver for Terraform {
    /// The state a failed apply left proves nothing ready (#113).
    fn ready_by_observation(&self, _failed_start: bool) -> bool {
        false
    }

    async fn probe(&self, cx: &ToolCx<'_>) -> Result<()> {
        let module = Module::of(cx);
        let found = tf::version(&module.call(READ_LIMIT, &super::discard))
            .await
            .map_err(|e| module.error("probe", e))?;
        super::unknown_version(cx, found.as_ref(), "terraform", tf::MIN_VERSION);
        Ok(())
    }

    async fn up(&self, cx: &ToolCx<'_>) -> Result<()> {
        required::refuse_unset(cx)?;
        let module = Module::of(cx);
        cx.steps.start(Phase::Init, None);
        let state = module.init().await?;
        cx.steps.done(Phase::Init, None);
        cx.steps.start(Phase::Apply, None);
        jump::through(&module, async |m| {
            m.apply("apply", state, tf::Change::Apply).await.map_err(|e| m.error("apply", e))
        })
        .await?;
        cx.steps.end(true);
        Ok(())
    }

    async fn down(&self, cx: &ToolCx<'_>) -> Result<()> {
        let module = Module::of(cx);
        let state = module.init().await?;
        jump::through(&module, async |m| m.destroy(state).await).await?;
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
        jump::through(&module, async |m| {
            m.apply("apply", state, tf::Change::Apply).await.map_err(|e| m.error("apply", e))
        })
        .await
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

    /// An `ssh` into the VM that runs the command, along the route the module's outputs and the
    /// run's values give ([`console::exec`]).
    async fn exec(&self, cx: &ToolCx<'_>, machine: &str, argv: &[String]) -> Result<Ran> {
        console::exec(&Module::of(cx), machine, argv).await
    }

    /// An `ssh` into the VM, along the route the module's outputs and the run's values give, as
    /// the user asked for or the one the outputs name, with the run's key ([`console::of`]).
    async fn console(&self, cx: &ToolCx<'_>, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        console::of(&Module::of(cx), machine, user).await
    }

    async fn observe(&self, cx: &ToolCx<'_>) -> Result<(Seen, RunState)> {
        let module = Module::of(cx);
        let state = module.state()?;
        let held = module.shown(state).await?;
        let outputs = match state {
            StateAt::File(file) if !file.exists() => std::collections::BTreeMap::new(),
            _ => tf::outputs(&module.call(READ_LIMIT, &super::discard), state)
                .await
                .map_err(|e| module.error("output", e))?,
        };
        let mut machines = machines_of(&held.managed);
        with_named_addresses(&mut machines, &outputs);
        screen::on_server(cx, &outputs, &held.managed, &mut machines).await?;
        let seen = Seen {
            machines,
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
        Ok((seen, run_state(&held)))
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
        let vm = vm_named(cx, &held, machine)?;
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

    /// The `published` output's entries (`{ "web/site" = "192.0.2.10:8080" }`), each one whose
    /// address has a host and a port; one without its address yet (an empty host) is left out.
    async fn published(&self, cx: &ToolCx<'_>) -> Result<Vec<super::Published>> {
        let module = Module::of(cx);
        let out = tunnelled::outputs(&module, module.state()?).await?;
        Ok(tunnelled::published(&out))
    }
}

#[cfg(test)]
pub(crate) mod tests;
