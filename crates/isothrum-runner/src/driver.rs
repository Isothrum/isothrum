//! Drivers: the manifest's closed set (`compose`, `vagrant`, `terraform`), dispatched through
//! [`AnyDriver`]. Steps are generic and run by the executor; a driver implements only its
//! documented actions (run-manifest-v0.md §6), how it reads status, and its run-state upkeep.

mod compose;
pub(crate) mod console;
mod terraform;
mod vagrant;

pub(crate) use compose::read_ps;
pub(crate) use terraform::plugin_cache;

use std::collections::BTreeMap;

use isoloom_manifest::{Action, Delivery, Identity, Machine, Manifest, SnakeName, Step, Template, Transport};
use tracing::Instrument;

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::status::{MachineStatus, ShellCommand};
use crate::vars::PortKey;

/// The env var naming the host ports a run forwards from this computer, as a JSON object
/// `{"<machine>/<port>": <host port>}`. A driver that chooses its own host ports (Vagrant,
/// locally) sets it on a fresh start; the Vagrantfile reads it, defaulting each to the port's
/// `publish` number.
pub(crate) const HOSTPORTS: &str = "ISOLOOM_HOSTPORTS";

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
    /// Hot-adds the lab's on-demand access machine into the running run, by the native name its
    /// engine addresses it by (the Compose service, the Vagrant define), which the initial `up`
    /// left down: brought up with the learner-picked `image`, else its baked default, on
    /// `network` when given.
    async fn attach(&self, cx: &Cx<'_>, native: &str, image: Option<&str>, network: Option<&str>) -> Result<()>;
    /// Hot-removes the lab's on-demand access machine `native` from the running run.
    async fn detach(&self, cx: &Cx<'_>, native: &str) -> Result<()>;
    /// The argv that opens an interactive shell into the access machine `native`. A pure query:
    /// no process runs here, the embedder runs what it returns in its own terminal.
    fn shell_command(&self, cx: &Cx<'_>, native: &str) -> Result<ShellCommand>;
    /// Takes `machine` out of the running run, ahead of its removal from the manifest, where the
    /// next update of the run wouldn't remove it itself. Nothing to do by default: Compose's
    /// start removes the services the lab no longer has, and Terraform's apply what it no
    /// longer makes.
    async fn retire(&self, cx: &Cx<'_>, machine: &isoloom_manifest::Machine) -> Result<()> {
        let _ = (cx, machine);
        Ok(())
    }
    /// The argv that opens an interactive console into `machine`, any machine of the run. A pure
    /// query, as [`Driver::shell_command`]. Refused where the driver has no way in yet.
    fn console_command(&self, cx: &Cx<'_>, machine: &isoloom_manifest::Machine) -> Result<ShellCommand> {
        let _ = cx;
        Err(Error::Unsupported {
            what: format!("a console into `{}` on this target", machine.name),
        })
    }
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
                Delivery::Env { .. } | Delivery::StateFile { .. } => true,
                Delivery::TfVar { .. } => terraform,
                Delivery::Guest { transport, .. } => *transport == Transport::Ssh,
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
    pub(crate) fn forward_ports(&self, cx: &Cx<'_>) -> BTreeMap<String, u16> {
        match self {
            AnyDriver::Vagrant(_) => vagrant::pick_forwards(cx),
            AnyDriver::Compose(_) | AnyDriver::Terraform(_) => BTreeMap::new(),
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

    pub(crate) fn after_down(&self, cx: &Cx<'_>) -> Result<()> {
        match self {
            AnyDriver::Compose(d) => d.after_down(cx),
            AnyDriver::Vagrant(d) => d.after_down(cx),
            AnyDriver::Terraform(d) => d.after_down(cx),
        }
    }

    pub(crate) async fn attach(&self, cx: &Cx<'_>, native: &str, image: Option<&str>, network: Option<&str>) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = "attach");
        match self {
            AnyDriver::Compose(d) => d.attach(cx, native, image, network).instrument(span).await,
            AnyDriver::Vagrant(d) => d.attach(cx, native, image, network).instrument(span).await,
            AnyDriver::Terraform(d) => d.attach(cx, native, image, network).instrument(span).await,
        }
    }

    pub(crate) async fn detach(&self, cx: &Cx<'_>, native: &str) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = "detach");
        match self {
            AnyDriver::Compose(d) => d.detach(cx, native).instrument(span).await,
            AnyDriver::Vagrant(d) => d.detach(cx, native).instrument(span).await,
            AnyDriver::Terraform(d) => d.detach(cx, native).instrument(span).await,
        }
    }

    pub(crate) fn shell_command(&self, cx: &Cx<'_>, native: &str) -> Result<ShellCommand> {
        match self {
            AnyDriver::Compose(d) => d.shell_command(cx, native),
            AnyDriver::Vagrant(d) => d.shell_command(cx, native),
            AnyDriver::Terraform(d) => d.shell_command(cx, native),
        }
    }

    pub(crate) async fn retire(&self, cx: &Cx<'_>, machine: &isoloom_manifest::Machine) -> Result<()> {
        match self {
            AnyDriver::Compose(d) => d.retire(cx, machine).await,
            AnyDriver::Vagrant(d) => d.retire(cx, machine).await,
            AnyDriver::Terraform(d) => d.retire(cx, machine).await,
        }
    }

    pub(crate) fn console_command(&self, cx: &Cx<'_>, machine: &isoloom_manifest::Machine) -> Result<ShellCommand> {
        match self {
            AnyDriver::Compose(d) => d.console_command(cx, machine),
            AnyDriver::Vagrant(d) => d.console_command(cx, machine),
            AnyDriver::Terraform(d) => d.console_command(cx, machine),
        }
    }
}

/// Refuses an action of another driver's (the manifest's actions are each one driver's): step
/// `step` of a `driver` run.
pub(crate) fn foreign_action(driver: &str, step: &Step, action: Action) -> Error {
    Error::Unsupported {
        what: format!("step `{}`: the action `{}` on a {driver} run", step.id, action_name(action)),
    }
}

/// What the manifest's `lifecycle.status` prints: the engine's own report of the run.
pub(crate) async fn status_report(cx: &Cx<'_>) -> Result<String> {
    crate::executor::capture_step(cx, "lifecycle.status", &cx.run.manifest().lifecycle.status).await
}

/// A machine's static addresses as the status shows them: each lab network's name to the address
/// the manifest fixes on it.
pub(crate) fn static_addresses(m: &Machine) -> BTreeMap<String, String> {
    m.addresses.iter().map(|(n, a)| (n.to_string(), a.to_string())).collect()
}

/// An action's name, as the manifest writes it.
pub(crate) fn action_name(action: Action) -> String {
    serde_json::to_value(action)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
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
        assert_eq!(
            AnyDriver::for_manifest(&m).unwrap_err().to_string(),
            "a `tf_var` delivery isn't supported by this runner yet"
        );
        let mut cloud = crate::testing::proxmox_manifest();
        let file: Delivery = serde_json::from_str(r#"{"via": "state_file", "path": "inputs.tfvars.json", "format": "tfvars_json"}"#).unwrap();
        cloud.secrets[0].deliver = isoloom_manifest::NonEmpty::one(file);
        assert!(AnyDriver::for_manifest(&cloud).is_ok(), "a state file");
        let winrm: Delivery = serde_json::from_str(r#"{"via": "guest", "machine": "web", "transport": "winrm"}"#).unwrap();
        cloud.secrets[0].deliver = isoloom_manifest::NonEmpty::one(winrm);
        assert_eq!(
            AnyDriver::for_manifest(&cloud).unwrap_err().to_string(),
            "a `guest` delivery isn't supported by this runner yet",
            "over SSH only"
        );
    }
}
