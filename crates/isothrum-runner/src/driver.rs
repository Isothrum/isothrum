//! Drivers: the manifest's closed set (`compose`, `vagrant`, `terraform`), dispatched through
//! [`AnyDriver`]. Steps are generic and run by the executor; a driver implements only its
//! actions (each one a run manifest's step names is one driver's, and [`foreign_action`] refuses
//! another driver's), how it reads status, and its run-state upkeep.

mod compose;
mod port_pick;
pub(crate) mod shells;
pub(crate) mod terraform;
mod vagrant;

pub(crate) use compose::{UNREADABLE_PS, read_ps};
pub(crate) use terraform::plugin_cache;

use std::collections::BTreeMap;

use isoloom_manifest::{Action, Delivery, Identity, Machine, Manifest, Native, SnakeName, Step, Template};
use tracing::Instrument;

use crate::Result;
use crate::console::ShellCommand;
use crate::encoding::wire_name;
use crate::error::Error;
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::status::MachineStatus;
use crate::vars::PortKey;

/// The label Compose puts on each container naming its service, which finds a service's
/// container whatever project and file started it.
pub(crate) const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// How long taking one machine out of a running run may go without progress (as every tool
/// call's limit counts): a VM destroyed, a service in the run's VM removed with its namespace.
pub(crate) const REMOVE_LIMIT: std::time::Duration = std::time::Duration::from_secs(300);

/// What the engine says about a run.
#[derive(Debug, Clone, Default)]
pub(crate) struct Observed {
    /// Every machine of the manifest.
    pub(crate) machines: Vec<MachineStatus>,
    /// The host port each published machine port got.
    pub(crate) published: BTreeMap<PortKey, u16>,
    /// Where each machine's published ports answer (`${host.<machine>}`).
    pub(crate) hosts: BTreeMap<String, String>,
    /// What the engine gives back, by output name (a Terraform state's outputs).
    pub(crate) outputs: BTreeMap<String, String>,
    /// The engine's outputs it marks sensitive itself (a Terraform output's `sensitive`).
    pub(crate) sensitive: std::collections::BTreeSet<String>,
}

/// A driver. Internal and statically dispatched (`async fn`, not object safe by design).
pub(crate) trait Driver {
    /// Refuses a start this driver can see won't work, before anything is created (Vagrant: a
    /// provider the run refuses, or whose plugin is missing).
    async fn preflight(&self, cx: &Cx<'_>) -> Result<()>;
    /// Readies the run's state before any of its steps (Compose: the pinned-ports file exists).
    fn prepare(&self, cx: &Cx<'_>) -> Result<()>;
    /// What every process of the run gets in its environment, besides its step's (Vagrant: its
    /// state folder, `VAGRANT_DOTFILE_PATH`).
    fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>>;
    /// Runs one of this driver's actions.
    async fn action(&self, cx: &Cx<'_>, step: &Step, action: Action, with: &BTreeMap<SnakeName, Template>) -> Result<()>;
    /// The run as the engine reports it, from the manifest's `lifecycle.status`.
    async fn observe(&self, cx: &Cx<'_>) -> Result<Observed>;
    /// Clears the run's state after a successful `down` (Compose: no pins left).
    fn after_down(&self, cx: &Cx<'_>) -> Result<()>;
    /// Restarts `machine`, running, as step `id`, so it boots with the settings its engine
    /// applies only at boot (an update, when its boot digest changed: [`Native::Vagrant`]'s
    /// `boot_digest`). Nothing for a driver whose machines carry no boot digest.
    async fn restart(&self, cx: &Cx<'_>, machine: &Machine, id: &str) -> Result<()>;
    /// Hot-adds `machine`, an on-demand access machine the initial `up` left down, into the
    /// running run: brought up with the picked `image`, else its baked default, and joined to the
    /// network whose native name is `network` when one is given. Refused where the driver can't.
    async fn attach(&self, cx: &Cx<'_>, machine: &Machine, image: Option<&str>, network: Option<&str>) -> Result<()>;
    /// Takes `machine` out of the running run (a retire ahead of its removal from the manifest,
    /// or its move out of the default start; a detach of an on-demand access machine), where the
    /// next update wouldn't remove it itself. The operation's step is named after the emitter's
    /// op. Refused where the driver does nothing for it, except for a machine a Terraform module
    /// makes, which the update's apply removes. A Compose service in the run's VM never reaches
    /// here: [`crate::in_vm::retire`] takes it.
    async fn retire(&self, cx: &Cx<'_>, machine: &Machine) -> Result<()>;
    /// The argv that opens an interactive console into `machine`, any machine of the run. A pure
    /// query: no process runs here, the embedder runs what it returns in its own terminal.
    /// Refused where the driver has no way in. A Compose service in the run's VM never reaches
    /// here: [`crate::in_vm::console`] opens it.
    fn console_command(&self, cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand>;
    /// The process that runs `script` inside `vm`, the VM the run's containers run in
    /// (`docker-vm`'s guest), through the VM's way in, as `use_` says. Refused by a driver whose
    /// runs have no such VM.
    fn in_guest(&self, cx: &Cx<'_>, vm: &Machine, script: &str, use_: GuestUse) -> Result<ProcessSpec>;
}

/// What a script run in the run's VM ([`Driver::in_guest`]) is for, which decides how it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestUse {
    /// A console the embedder runs in its own terminal: with a terminal, and with the run's own
    /// variables only. Nothing the manifest delivers to a step goes in: the embedder is handed
    /// the command, its variables included, so a secret delivered there would show.
    Console,
    /// Part of the operation, run by the runner itself: no terminal, with what the manifest
    /// delivers to the operation's step (the values reach only the process).
    Step,
}

/// The driver a manifest names.
#[derive(Debug, Clone)]
pub(crate) enum AnyDriver {
    Compose(compose::Compose),
    Vagrant(vagrant::Vagrant),
    Terraform(terraform::Terraform),
}

impl AnyDriver {
    /// The driver for `manifest`, refusing what this runner doesn't run yet.
    pub(crate) fn for_manifest(manifest: &Manifest) -> Result<AnyDriver> {
        let unsupported = |what: String| Err(Error::Unsupported { what });
        let driver = match &manifest.identity {
            Identity::Compose(identity) => AnyDriver::Compose(compose::Compose::new(identity.clone())),
            Identity::Vagrant(identity) => AnyDriver::Vagrant(vagrant::Vagrant::new(identity.clone())),
            Identity::Terraform(identity) => AnyDriver::Terraform(terraform::Terraform::new(identity.clone())),
        };
        let deliveries = manifest
            .params
            .iter()
            .flat_map(|p| p.deliver.iter())
            .chain(manifest.secrets.iter().flat_map(|s| s.deliver.iter()));
        let terraform = matches!(driver, AnyDriver::Terraform(_));
        for delivery in deliveries {
            let ok = match delivery {
                Delivery::Env { .. } | Delivery::StateFile { .. } | Delivery::Guest { .. } => true,
                Delivery::TfVar { .. } => terraform,
            };
            if !ok {
                return unsupported(format!("a `{}` delivery", delivery_kind(delivery)));
            }
        }
        Ok(driver)
    }

    /// The driver's name, for tracing.
    fn name(&self) -> &'static str {
        match self {
            AnyDriver::Compose(_) => "compose",
            AnyDriver::Vagrant(_) => "vagrant",
            AnyDriver::Terraform(_) => "terraform",
        }
    }

    pub(crate) async fn preflight(&self, cx: &Cx<'_>) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), check = "preflight");
        match self {
            AnyDriver::Compose(d) => d.preflight(cx).instrument(span).await,
            AnyDriver::Vagrant(d) => d.preflight(cx).instrument(span).await,
            AnyDriver::Terraform(d) => d.preflight(cx).instrument(span).await,
        }
    }

    /// Whether the run's management commands need elevation in `scope` (Hyper-V's provider).
    pub(crate) fn elevates(&self, scope: &crate::scope::Scope) -> bool {
        match self {
            AnyDriver::Compose(_) | AnyDriver::Terraform(_) => false,
            AnyDriver::Vagrant(d) => d.elevates(scope),
        }
    }

    pub(crate) fn prepare(&self, cx: &Cx<'_>) -> Result<()> {
        match self {
            AnyDriver::Compose(d) => d.prepare(cx),
            AnyDriver::Vagrant(d) => d.prepare(cx),
            AnyDriver::Terraform(d) => d.prepare(cx),
        }
    }

    pub(crate) fn env(&self, cx: &Cx<'_>) -> Result<Vec<(String, String)>> {
        match self {
            AnyDriver::Compose(d) => d.env(cx),
            AnyDriver::Vagrant(d) => d.env(cx),
            AnyDriver::Terraform(d) => d.env(cx),
        }
    }

    /// The host ports a fresh start pins for a driver that chooses its own (Vagrant, locally: a
    /// free loopback port per ephemeral published port, by `"<machine>/<port>"`), empty for the
    /// others. Recorded so a resume forwards the same ports; Compose pins its own through its
    /// `pin_ports` action, and a Terraform (server/cloud) run forwards nothing to this computer.
    ///
    /// # Errors
    ///
    /// `tool_failed` when no free host port is left for one.
    pub(crate) fn forward_ports(&self, cx: &Cx<'_>) -> Result<BTreeMap<String, u16>> {
        match self {
            AnyDriver::Vagrant(_) => vagrant::forwarded::pick(cx),
            AnyDriver::Compose(_) | AnyDriver::Terraform(_) => Ok(BTreeMap::new()),
        }
    }

    pub(crate) async fn action(&self, cx: &Cx<'_>, step: &Step, action: Action, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), action = ?action);
        match self {
            AnyDriver::Compose(d) => d.action(cx, step, action, with).instrument(span).await,
            AnyDriver::Vagrant(d) => d.action(cx, step, action, with).instrument(span).await,
            AnyDriver::Terraform(d) => d.action(cx, step, action, with).instrument(span).await,
        }
    }

    pub(crate) async fn observe(&self, cx: &Cx<'_>) -> Result<Observed> {
        let span = tracing::debug_span!("driver", driver = self.name(), read = "status");
        match self {
            AnyDriver::Compose(d) => d.observe(cx).instrument(span).await,
            AnyDriver::Vagrant(d) => d.observe(cx).instrument(span).await,
            AnyDriver::Terraform(d) => d.observe(cx).instrument(span).await,
        }
    }

    /// What a failed `down` may print for each resource of the run that is already gone, and the
    /// resource: Terraform's `identity.terraform.gone`, filled for each instance its state holds
    /// ([`terraform::gone`]). None for the other drivers.
    pub(crate) fn gone(&self, cx: &Cx<'_>) -> Result<Vec<terraform::gone::Gone>> {
        match self {
            AnyDriver::Terraform(d) => d.gone(cx),
            AnyDriver::Compose(_) | AnyDriver::Vagrant(_) => Ok(Vec::new()),
        }
    }

    /// Drops from the run's state each resource [`AnyDriver::gone`] names whose text, among
    /// `seen`, step `step` of list `list` printed before it failed, through that step's own tool;
    /// whether it dropped any (so the list may run again). Nothing for a step that isn't a
    /// Terraform destroy, nor for another driver.
    pub(crate) async fn forget_gone(&self, cx: &Cx<'_>, list: &str, step: &Step, seen: &std::collections::BTreeSet<String>) -> Result<bool> {
        match self {
            AnyDriver::Terraform(d) => d.forget_gone(cx, list, step, seen).await,
            AnyDriver::Compose(_) | AnyDriver::Vagrant(_) => Ok(false),
        }
    }

    pub(crate) fn after_down(&self, cx: &Cx<'_>) -> Result<()> {
        match self {
            AnyDriver::Compose(d) => d.after_down(cx),
            AnyDriver::Vagrant(d) => d.after_down(cx),
            AnyDriver::Terraform(d) => d.after_down(cx),
        }
    }

    pub(crate) async fn restart(&self, cx: &Cx<'_>, machine: &Machine, id: &str) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = cx.events.call_id(), machine = %machine.name);
        match self {
            AnyDriver::Compose(d) => d.restart(cx, machine, id).instrument(span).await,
            AnyDriver::Vagrant(d) => d.restart(cx, machine, id).instrument(span).await,
            AnyDriver::Terraform(d) => d.restart(cx, machine, id).instrument(span).await,
        }
    }

    pub(crate) async fn attach(&self, cx: &Cx<'_>, machine: &Machine, image: Option<&str>, network: Option<&str>) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = "attach", machine = %machine.name);
        match self {
            AnyDriver::Compose(d) => d.attach(cx, machine, image, network).instrument(span).await,
            AnyDriver::Vagrant(d) => d.attach(cx, machine, image, network).instrument(span).await,
            AnyDriver::Terraform(d) => d.attach(cx, machine, image, network).instrument(span).await,
        }
    }

    pub(crate) async fn retire(&self, cx: &Cx<'_>, machine: &Machine) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = cx.events.call_id(), machine = %machine.name);
        match self {
            AnyDriver::Compose(d) => d.retire(cx, machine).instrument(span).await,
            AnyDriver::Vagrant(d) => d.retire(cx, machine).instrument(span).await,
            AnyDriver::Terraform(d) => d.retire(cx, machine).instrument(span).await,
        }
    }

    pub(crate) fn console_command(&self, cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand> {
        let _span = tracing::debug_span!("driver", driver = self.name(), query = "console", machine = %machine.name).entered();
        match self {
            AnyDriver::Compose(d) => d.console_command(cx, machine),
            AnyDriver::Vagrant(d) => d.console_command(cx, machine),
            AnyDriver::Terraform(d) => d.console_command(cx, machine),
        }
    }

    pub(crate) fn in_guest(&self, cx: &Cx<'_>, vm: &Machine, script: &str, use_: GuestUse) -> Result<ProcessSpec> {
        match self {
            AnyDriver::Compose(d) => d.in_guest(cx, vm, script, use_),
            AnyDriver::Vagrant(d) => d.in_guest(cx, vm, script, use_),
            AnyDriver::Terraform(d) => d.in_guest(cx, vm, script, use_),
        }
    }
}

/// The console `spec` builds, run in its own folder (`ssh`'s: the run's state folder, which its
/// config is named relative to), else in the run's folder.
pub(crate) fn console_in_run(cx: &Cx<'_>, spec: ProcessSpec) -> ShellCommand {
    let cwd = spec.cwd.unwrap_or_else(|| cx.run.dir().to_path_buf());
    ShellCommand::new(spec.program, spec.args, cwd).with_env(spec.env)
}

/// Refuses an action of another driver's (the manifest's actions are each one driver's): step
/// `step` of a `driver` run.
pub(crate) fn foreign_action(driver: &str, step: &Step, action: Action) -> Error {
    Error::Unsupported {
        what: format!("step `{}`: the action `{}` on a {driver} run", step.id, wire_name(&action)),
    }
}

/// The manifest's `lifecycle.status`, as messages name it.
pub(crate) const STATUS: &str = "lifecycle.status";

/// What the manifest's `lifecycle.status` prints: the engine's own report of the run.
pub(crate) async fn status_report(cx: &Cx<'_>) -> Result<String> {
    crate::executor::capture_step(cx, STATUS, &cx.run.manifest().lifecycle.status).await
}

/// A machine's static addresses as the status shows them: each of the run's networks, by name, to the address
/// the manifest fixes on it.
pub(crate) fn static_addresses(m: &Machine) -> BTreeMap<String, String> {
    m.addresses.iter().map(|(n, a)| (n.to_string(), a.to_string())).collect()
}

/// The name the machine's engine addresses it by: its Compose service, its Vagrant define, else
/// its manifest name.
pub(crate) fn native_name(m: &Machine) -> &str {
    match &m.native {
        Some(Native::Compose { service }) => service.as_str(),
        Some(Native::Vagrant { vagrant, .. }) => vagrant.as_str(),
        None => m.name.as_str(),
    }
}

/// Whether `m` is a Compose service.
pub(crate) fn is_compose_service(m: &Machine) -> bool {
    matches!(m.native, Some(Native::Compose { .. }))
}

/// The refusal of `what` (`taking out`, `a console into`) on `machine`, a Compose service on a
/// run with no VM for it to run in.
pub(crate) fn no_vm_for(what: &str, machine: &Machine) -> Error {
    Error::Unsupported {
        what: format!("{what} `{}`: a Compose service with no VM to run in", machine.name),
    }
}

fn delivery_kind(d: &Delivery) -> &'static str {
    match d {
        Delivery::Env { .. } => "env",
        Delivery::TfVar { .. } => "tf_var",
        Delivery::StateFile { .. } => "state_file",
        Delivery::Guest { .. } => "guest",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_what_this_runner_runs_is_accepted() {
        assert!(matches!(AnyDriver::for_manifest(&crate::testing::docker_manifest()), Ok(AnyDriver::Compose(_))));
        assert!(matches!(
            AnyDriver::for_manifest(&crate::testing::vagrant_manifest()),
            Ok(AnyDriver::Vagrant(_))
        ));
        assert!(matches!(
            AnyDriver::for_manifest(&crate::testing::proxmox_manifest()),
            Ok(AnyDriver::Terraform(_))
        ));
        let mut m = crate::testing::docker_manifest();
        let tf: Delivery = serde_json::from_str(r#"{"via": "tf_var", "var": "inputs"}"#).unwrap();
        m.secrets[0].deliver = isoloom_manifest::NonEmpty::one(tf);
        assert_eq!(AnyDriver::for_manifest(&m).unwrap_err().to_string(), "a `tf_var` delivery isn't supported");
        let mut cloud = crate::testing::proxmox_manifest();
        let file: Delivery = serde_json::from_str(r#"{"via": "state_file", "path": "inputs.tfvars.json", "format": "tfvars_json"}"#).unwrap();
        cloud.secrets[0].deliver = isoloom_manifest::NonEmpty::one(file);
        assert!(AnyDriver::for_manifest(&cloud).is_ok(), "a state file");
    }
}
