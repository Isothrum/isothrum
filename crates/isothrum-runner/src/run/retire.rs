//! Taking a machine out of a running run ahead of the update that drops it: what that update
//! wouldn't remove itself. The detach of an on-demand access machine takes it out the same way.

use isoloom_manifest::{Machine, Native};

use super::Running;
use super::operation::Done;
use crate::Result;
use crate::error::Error;
use crate::events::{Emitter, Events};
use crate::executor::Cx;
use crate::options::RunOptions;
use crate::state::Op;

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
        self.operate(Op::Retire, opts, events, async |connected: &RunOptions, e: &Emitter<'_>| {
            let target = self.machine_named(machine)?;
            if self.manifest().guest.as_ref().is_some_and(|g| g.machine == target.name) {
                return Err(Error::Unsupported {
                    what: format!("retiring `{machine}`, the VM the run's containers run in"),
                });
            }
            let (_, cx) = self.existing(connected, e).await?;
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
}
