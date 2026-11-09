//! Bringing a running run up to date with its manifest. The embedder regenerated the manifest
//! (a machine added to the lab or taken out of it, a set-up changed) and opened the run on it;
//! `up`'s steps run again against what is up, with the params the run was started with, so each
//! engine converges: Terraform makes and removes what changed, Compose starts new services,
//! Vagrant brings up new VMs. Then, as at `up`, the guests' secrets, the ready conditions, the
//! outputs and the message.
//!
//! What an update won't remove itself the embedder retires before calling it
//! ([`Running::retire`]): a VM taken out (Vagrant forgets a define the Vagrantfile no longer
//! has), a Compose service taken out or moved out of the default start.
//!
//! A machine that was running keeps what it was given: the guests' secrets go to new machines
//! only (an update carries no launch's values, which a run never keeps).
//!
//! A sealed run stays sealed: its recorded seal is replayed, so a machine added to an
//! `internet: false` network sets itself up without the internet.
//!
//! A running VM whose boot-time settings changed is restarted first ([`super::boot`]).

use super::Running;
use super::operation::Done;
use super::up::ready_run;
use crate::Result;
use crate::events::{Emitter, Events};
use crate::options::RunOptions;
use crate::secrets::check_secrets;
use crate::state::{Op, RunState};
use crate::status::{MachineState, Started};
use crate::{ready, tools};

impl Running<'_> {
    /// Brings the running run up to date with its manifest (see the module doc): `up`'s steps
    /// again, with the run's recorded params and `opts`' secrets, then what follows `up`.
    ///
    /// # Errors
    ///
    /// Fails with `not_running` when no target machine runs, when a secret or tool is missing,
    /// and as `up` does for a step, a ready condition or the run's state.
    pub async fn update(&self, opts: &RunOptions, events: &dyn Events) -> Result<Started> {
        self.operate(Op::Update, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            self.update_work(connected, e).await
        })
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
        self.reload_changed(&cx, &record.vm_boot_digests, &observed).await?;
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
}
