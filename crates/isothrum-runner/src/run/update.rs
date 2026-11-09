//! Bringing a running run up to date with its manifest. The embedder regenerated the manifest
//! (a machine added to the lab or taken out of it, a set-up changed) and opened the run on it;
//! `up`'s steps run again against what is up, with the params the run was started with, so each
//! engine converges: Terraform makes and removes what changed, Compose starts new services,
//! Vagrant brings up new VMs. What an update doesn't remove itself is retired first
//! ([`Running::retire`]): a VM taken out (Vagrant forgets a define the Vagrantfile no longer
//! has), a Compose service taken out or moved out of the default start.
//! Then, as at `up`, the guests' secrets, the ready conditions, the outputs and the message.
//!
//! A machine that was running keeps what it was given: the guests' secrets go to new machines
//! only (an update carries no launch's values, which a run never keeps).
//!
//! A sealed run stays sealed: its recorded seal is replayed, so a machine added to an
//! `internet: false` network sets itself up without the internet.
//!
//! A running VM whose boot-time settings changed is restarted first. Vagrant applies a VM's
//! synced folders, networks, forwarded ports, size and provider settings only when it boots it;
//! `up`'s steps on a running VM would leave them as they were (a host folder added: the VM never
//! mounts it). The manifest's `native.boot_digest` of each VM is compared with the one the run
//! recorded at its last `up` or update: a running VM whose two digests are both known and differ
//! is reloaded ([`AnyDriver::reload`](crate::driver::AnyDriver::reload)) before `up`'s steps run
//! again, which then provision it and start what runs in it (a `docker-vm`'s containers). A VM
//! with no recorded digest (a run recorded before the runner kept them) or none in the manifest
//! is left running: an unknown digest never reloads. Either way the update records the
//! manifest's digests.

use std::collections::BTreeMap;

use isoloom_manifest::{Machine, Manifest, Native};

use super::Running;
use super::operation::Done;
use super::up::ready_run;
use crate::Result;
use crate::driver::Observed;
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::executor::Cx;
use crate::options::RunOptions;
use crate::secrets::{check_secrets, sensitive_values};
use crate::state::{Op, RunState};
use crate::status::{MachineState, Started};
use crate::{ready, tools};

impl Running<'_> {
    /// Takes machine `machine` out of the running run, ahead of its removal from the manifest
    /// (then an update), or of its move out of the lab's start (back to an on-demand machine):
    /// what the next update wouldn't remove itself. A Vagrant VM is destroyed (Vagrant forgets a
    /// define the Vagrantfile no longer has); a Compose service is stopped and removed with the
    /// services in its namespace, on this computer or inside the run's VM (an update leaves a
    /// service outside the default start running). A machine a Terraform module makes is left to
    /// the update's apply, which removes what the module no longer makes: the retire runs nothing
    /// and succeeds.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have; `unsupported` for the VM
    /// the run's containers run in (`docker-vm`'s guest: every container would go with it), and
    /// for a machine the run has nothing to take out of (one whose manifest doesn't say how it
    /// runs, a Compose service with no VM to run in, an on-demand machine on a server or cloud);
    /// and as the other operations do for a step or the run's state.
    pub async fn retire(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Retire, events, mask, async |e: &Emitter<'_>| {
            let target = self.machine_named(machine)?;
            if self.manifest().guest.as_ref().is_some_and(|g| g.machine == target.name) {
                return Err(Error::Unsupported {
                    what: format!("retiring `{machine}`, the VM the run's containers run in"),
                });
            }
            let (_, cx) = self.existing(opts, e).await?;
            self.take_out(&cx, target).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: serde_json::json!({ "machine": machine }),
                state: None,
            })
        })
        .await
    }

    /// Takes `target` out of the running run: a Compose service in the run's VM there
    /// ([`crate::in_vm::retire`]), anything else through its driver.
    pub(crate) async fn take_out(&self, cx: &Cx<'_>, target: &Machine) -> Result<()> {
        match (&self.manifest().guest, &target.native) {
            (Some(guest), Some(Native::Compose { service })) => crate::in_vm::retire(cx, &self.driver, guest, service.as_str()).await,
            _ => self.driver.retire(cx, target).await,
        }
    }

    /// Brings the running run up to date with its manifest (see the module doc): `up`'s steps
    /// again, with the run's recorded params and `opts`' secrets, then what follows `up`.
    ///
    /// # Errors
    ///
    /// Fails with `not_running` when no target machine runs, when a secret or tool is missing,
    /// and as `up` does for a step, a ready condition or the run's state.
    pub async fn update(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        let opts = &self.connected(opts);
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Update, events, mask, async |e: &Emitter<'_>| self.update_work(opts, e).await)
            .await
    }

    async fn update_work(&self, opts: &RunOptions, events: &Emitter<'_>) -> Result<Done<Started>> {
        check_secrets(self.manifest(), &opts.secrets, &self.scope(opts.provider.clone()))?;
        let (mut record, mut cx) = self.existing(opts, events).await?;
        tools::probe(&cx).await?;
        let observed = self.observe_running(&mut cx).await?;
        // What runs now keeps what it was given: only a machine new to the run gets its secrets.
        cx.set_up = observed
            .machines
            .iter()
            .filter(|m| m.state == MachineState::Running)
            .map(|m| m.name.clone())
            .collect();
        for vm in changed_at_boot(self.manifest(), &record.vm_boot_digests, &observed) {
            self.reload(&cx, vm).await?;
        }
        self.runs_from_this(&mut record);
        self.begin(&mut record, Op::Update)?;
        // The manifest may name new state files, or new entries in them (a new machine's inputs).
        crate::executor::state_files::write(&cx, true)?;
        let ran = self.start(&mut cx).await?;
        crate::guest::deliver(&mut cx, None).await?;
        self.read_outputs(&mut cx).await?;
        ready::check(&mut cx, Some(&ran)).await?;
        let (outputs, message) = self.finish(&mut cx).await?;
        record.state = RunState::Ready;
        Ok(ready_run(record, outputs, message))
    }

    /// Restarts `vm` so it boots with its new settings, as its own step (`reload-<machine>`).
    async fn reload(&self, cx: &Cx<'_>, vm: &Machine) -> Result<()> {
        let id = format!("reload-{}", vm.name);
        let note = format!("Restarting {} to apply its new settings…", vm.name);
        cx.events.stepped(&id, Some(&note), self.driver.reload(cx, vm, &id)).await
    }
}

/// Each VM's boot digest in `manifest` (its `native.boot_digest`), by machine name.
pub(crate) fn boot_digests(manifest: &Manifest) -> BTreeMap<String, String> {
    manifest
        .machines
        .iter()
        .filter_map(|m| match &m.native {
            Some(Native::Vagrant { boot_digest: Some(digest), .. }) => Some((m.name.to_string(), digest.clone())),
            _ => None,
        })
        .collect()
}

/// The machines of `manifest` that run (as `observed`) and whose boot digest differs from the
/// one `recorded`, both known.
fn changed_at_boot<'m>(manifest: &'m Manifest, recorded: &BTreeMap<String, String>, observed: &Observed) -> Vec<&'m Machine> {
    let now = boot_digests(manifest);
    let running = |name: &str| observed.machines.iter().any(|m| m.name == name && m.state == MachineState::Running);
    manifest
        .machines
        .iter()
        .filter(|m| {
            let name = m.name.as_str();
            matches!((recorded.get(name), now.get(name)), (Some(was), Some(is)) if was != is) && running(name)
        })
        .collect()
}
