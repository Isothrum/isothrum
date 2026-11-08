//! Bringing a running run up to date with its manifest. The embedder regenerated the manifest
//! (a machine added to the lab or taken out of it, a set-up changed) and opened the run on it;
//! `up`'s steps run again against what is up, with the params the run was started with, so each
//! engine converges: Terraform makes and removes what changed, Compose starts new services and
//! removes the ones gone (`--remove-orphans`), Vagrant brings up new VMs (a VM taken out is
//! retired first, [`Running::retire`]: Vagrant forgets a define the Vagrantfile no longer has).
//! Then, as at `up`, the guests' secrets, the ready conditions, the outputs and the message.
//!
//! A machine that was running keeps what it was given: the guests' secrets go to new machines
//! only (an update carries no launch's values, which a run never keeps).
//!
//! A sealed run stays sealed: its recorded seal is replayed, so a machine added to an
//! `internet: false` network sets itself up without the internet.

use super::Running;
use super::operation::Done;
use super::up::ready_run;
use crate::Result;
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::hooks::Edge;
use crate::options::RunOptions;
use crate::secrets::{check_secrets, sensitive_values};
use crate::state::{Op, RunState};
use crate::status::{MachineState, Started, any_target_running};
use crate::{ready, tools};

impl Running<'_> {
    /// Takes machine `machine` out of the running run, ahead of its removal from the manifest
    /// (then an update): what the next update wouldn't remove itself (a Vagrant VM, which Vagrant
    /// forgets once its define is gone). Nothing to do on the other targets.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have, and as the other
    /// operations do for a step or the run's state.
    pub async fn retire(&self, machine: &str, opts: &RunOptions, events: &dyn Events) -> Result<()> {
        let opts = &self.connected(opts);
        let target = self
            .manifest()
            .machines
            .iter()
            .find(|m| m.name.as_str() == machine)
            .ok_or_else(|| Error::MachineUnknown {
                run: self.name().to_string(),
                machine: machine.to_owned(),
            })?;
        let mask = sensitive_values(self.manifest(), &opts.secrets);
        self.operate(Op::Retire, events, mask, async |e: &Emitter<'_>| {
            let (_, cx) = self.existing(opts, e).await?;
            self.driver.retire(&cx, target).await?;
            Ok(Done {
                value: (),
                ok: true,
                data: serde_json::json!({ "machine": machine }),
                state: None,
            })
        })
        .await
    }

    /// Brings the running run up to date with its manifest (see the module doc): `up`'s steps
    /// again, with the run's recorded params and `opts`' secrets.
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
        let observed = self.observe(&mut cx).await?;
        if !any_target_running(&observed.machines) {
            return Err(Error::NotRunning { run: self.name().to_string() });
        }
        // What runs now keeps what it was given: only a machine new to the run gets its secrets.
        cx.set_up = observed
            .machines
            .iter()
            .filter(|m| m.state == MachineState::Running)
            .map(|m| m.name.clone())
            .collect();
        self.begin(&mut record, Op::Update)?;
        // The manifest may name new state files, or new entries in them (a new machine's inputs).
        crate::executor::state_files::write(&cx, true)?;
        let ran = self.start(&mut cx).await?;
        crate::guest::deliver(&mut cx, None).await?;
        self.read_outputs(&mut cx).await?;
        ready::check(&mut cx, Some(&ran)).await?;
        let (outputs, message) = self.finish(&mut cx, Edge::AfterUp).await?;
        record.state = RunState::Ready;
        Ok(ready_run(record, outputs, message))
    }
}
