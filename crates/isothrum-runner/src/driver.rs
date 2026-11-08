//! Drivers: the manifest's closed set (`compose`, `vagrant`, `terraform`), dispatched through
//! [`AnyDriver`]. Steps are generic and run by the executor; a driver implements only its
//! documented actions (run-manifest-v0.md §6), how it reads status, and its run-state upkeep.

mod compose;
mod terraform;
mod vagrant;
mod vm_box;

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

/// Which attacker box a hot-add acts on: the manifest's on-demand access machine when the lab
/// declares one, or an ad-hoc box the driver stands up itself with no machine behind it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Attacker<'a> {
    /// The manifest's on-demand access machine, by the native name its engine addresses it by
    /// (the Compose service, the Vagrant define). The initial `up` left it down.
    Machine(&'a str),
    /// No machine: the driver makes an attacker box ad hoc on the lab's network. A container on
    /// this computer's Docker (Compose), or inside the VM a container lab runs in (`docker-vm`,
    /// and Terraform's container host on a server or a cloud); a VM the runner adds to a VM lab's
    /// running environment (Vagrant). Terraform refuses it for a VM lab (`unsupported`): no VM
    /// to put it in.
    AdHoc,
}

/// Where a hot-added attacker box can be reached, as the driver stood it up. Filled for an ad-hoc
/// box the driver named and inspected; left empty for a manifest machine, whose identity and
/// address the caller already reads from the manifest.
#[derive(Debug, Clone, Default)]
pub(crate) struct Attached {
    /// The engine name the box came up under: an ad-hoc container's name, or the Vagrant define
    /// of the attack VM the runner adds.
    pub(crate) name: Option<String>,
    /// The lab network it landed on (the spec name).
    pub(crate) network: Option<String>,
    /// Its address on that network.
    pub(crate) address: Option<String>,
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
    /// Hot-adds the attacker box into the running run: an [`Attacker::Machine`] the manifest
    /// declares (brought up with the learner-picked `image`, else its baked default, on `network`
    /// when given), or an [`Attacker::AdHoc`] box the driver stands up itself with no machine
    /// behind it. Reports where an ad-hoc box can be reached ([`Attached`], empty for a machine).
    async fn attach(&self, cx: &Cx<'_>, attacker: Attacker<'_>, image: Option<&str>, network: Option<&str>) -> Result<Attached>;
    /// Hot-removes the attacker box from the running run.
    async fn detach(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<()>;
    /// The argv that opens an interactive shell into the attacker box. A pure query: no process
    /// runs here, the embedder runs what it returns in its own terminal.
    fn shell_command(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<ShellCommand>;
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

    pub(crate) async fn attach(&self, cx: &Cx<'_>, attacker: Attacker<'_>, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = "attach");
        match self {
            AnyDriver::Compose(d) => d.attach(cx, attacker, image, network).instrument(span).await,
            AnyDriver::Vagrant(d) => d.attach(cx, attacker, image, network).instrument(span).await,
            AnyDriver::Terraform(d) => d.attach(cx, attacker, image, network).instrument(span).await,
        }
    }

    pub(crate) async fn detach(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<()> {
        let span = tracing::debug_span!("driver", driver = self.name(), op = "detach");
        match self {
            AnyDriver::Compose(d) => d.detach(cx, attacker).instrument(span).await,
            AnyDriver::Vagrant(d) => d.detach(cx, attacker).instrument(span).await,
            AnyDriver::Terraform(d) => d.detach(cx, attacker).instrument(span).await,
        }
    }

    pub(crate) fn shell_command(&self, cx: &Cx<'_>, attacker: Attacker<'_>) -> Result<ShellCommand> {
        match self {
            AnyDriver::Compose(d) => d.shell_command(cx, attacker),
            AnyDriver::Vagrant(d) => d.shell_command(cx, attacker),
            AnyDriver::Terraform(d) => d.shell_command(cx, attacker),
        }
    }

    /// Removes, best effort, any ad-hoc attacker box this run left running, on teardown. The box is
    /// a bare container with no Compose project, so a lab's `down` (which removes the project's own
    /// containers) leaves it running; a workload scan then still groups it under the lab and reports
    /// the lab as running when nothing of it is. Only Compose stands an ad-hoc box up on this
    /// computer; a container lab in a VM keeps its box inside the VM, which `down` destroys with
    /// everything in it, and a run that never attached one has nothing to remove.
    pub(crate) async fn reap_attacker(&self, cx: &Cx<'_>) {
        if let AnyDriver::Compose(d) = self {
            let span = tracing::debug_span!("driver", driver = self.name(), op = "reap-attacker");
            drop(d.detach(cx, Attacker::AdHoc).instrument(span).await);
        }
    }
}

/// The param that names a run's attack box image when its module makes the box an instance of its
/// own (Isoloom's cloud-vm): empty for no box.
pub(crate) fn attack_vm_param() -> &'static str {
    terraform::attack_vm::PARAM
}

/// Whether an attach of `manifest`'s run adds an ad-hoc box: the lab declares no on-demand access
/// machine of its own. Attach and every driver's status decide it here, so a box an attach added
/// is always the one status shows (an always-up access machine doesn't stand in for it).
pub(crate) fn takes_adhoc_box(manifest: &isoloom_manifest::Manifest) -> bool {
    !manifest.machines.iter().any(|m| m.on_demand)
}

/// The name an ad-hoc attacker box reports under in status: Isoloom's reserved namespace, so it is
/// never a lab machine. Its container is `{project}-isoloom-access` on this computer's Docker
/// (Compose), `{run}-isoloom-access` inside a container lab's VM (Vagrant, and Terraform on a
/// server or a cloud); the VM the runner adds
/// to a VM lab is the Vagrant define `isoloom-access`, the name every Isoloom Vagrantfile's QEMU
/// hubs keep a port for.
pub(crate) const ADHOC_NAME: &str = "isoloom-access";

/// The hostname an attack box answers to inside (its shell prompt reads `root@attackbox`): the
/// learner's name for it, where [`ADHOC_NAME`] is Isoloom's internal one.
pub(crate) const ATTACK_HOSTNAME: &str = "attackbox";

/// The `docker inspect` template that reads an ad-hoc box's running flag and its networks, a `|`
/// apart: what [`adhoc_status`] reads.
pub(crate) const ADHOC_STATUS_FORMAT: &str = "{{.State.Running}}|{{json .NetworkSettings.Networks}}";

/// The ad-hoc attacker box as a status machine, from `out`, what `docker inspect` printed for
/// [`ADHOC_STATUS_FORMAT`]: running or stopped, its addresses by lab network as `addresses` reads
/// them from the networks JSON. `None` when the output isn't that (best effort: an inspect that
/// can't be read is simply no access machine).
pub(crate) fn adhoc_status(out: &str, addresses: impl FnOnce(&str) -> BTreeMap<String, String>) -> Option<MachineStatus> {
    let (running, networks) = out.trim().split_once('|')?;
    Some(MachineStatus {
        name: ADHOC_NAME.to_owned(),
        role: isoloom_manifest::Role::Access,
        state: if running.trim() == "true" {
            crate::status::MachineState::Running
        } else {
            crate::status::MachineState::Stopped
        },
        addresses: addresses(networks),
        published: Vec::new(),
    })
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
    #[test]
    fn an_ad_hoc_box_is_for_a_lab_with_no_on_demand_access_machine() {
        let parse = |text: &str| isoloom_manifest::parse(text).unwrap();
        assert!(super::takes_adhoc_box(&parse(crate::testing::FLAG_PORTAL)));
        assert!(!super::takes_adhoc_box(&parse(crate::testing::FLAG_PORTAL_ACCESS)));
        // An access machine that is always up doesn't stand in for the box: attach adds one, and
        // status must show it.
        let mut always_up = parse(crate::testing::FLAG_PORTAL_ACCESS);
        always_up.machines.iter_mut().for_each(|m| m.on_demand = false);
        assert!(super::takes_adhoc_box(&always_up));
    }

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
