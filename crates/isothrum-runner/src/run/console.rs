//! The way into a machine of a run: the command that opens a console into it, built by the
//! runner (it alone knows the runtime) and run by the embedder in its own terminal.

use isoloom_manifest::{Machine, Native};

use super::Running;
use crate::Result;
use crate::console::{ShellCommand, check_user};
use crate::executor::Cx;

impl Running<'_> {
    /// The command that opens an interactive console into `machine`, any machine of the run (an
    /// SSH login on a cloud's), by its native name (the Compose service, the Vagrant define), so
    /// the embedder never reconstructs a container name. A query: it runs nothing, emits nothing,
    /// writes only what the command needs (the SSH configuration in the run's state folder, for a
    /// way in over SSH; nothing for a container's), and doesn't check the machine is up (the embedder runs what it returns in its own
    /// terminal once the machine is up or attached).
    ///
    /// With `user`, the console opens as that account of the machine, in its home, rather than as
    /// the account the way in logs in as (a container's own user, Vagrant's `vagrant`, a module's
    /// SSH user): `exec --user` into a container, a `sudo -i` login on a VM (its way in's account
    /// may `sudo`, as a Vagrant box's and a cloud image's do). The machine must have the account.
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have, `not_running` when the
    /// run gives it no address yet, and `unsupported` where the target has no way in yet, for a
    /// `user` that can't name an account, and for a `user` on a Windows machine.
    pub fn console_command(&self, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
        let target = self.machine_named(machine)?;
        let user = user.map(|u| check_user(u, target)).transpose()?;
        self.query(|cx| self.console_of(cx, target, user))
    }

    /// The console into `target`, as `user` when given: into the run's VM, then into its
    /// container, for a Compose service in that VM ([`crate::in_vm::console`]); the driver's
    /// otherwise.
    fn console_of(&self, cx: &Cx<'_>, target: &Machine, user: Option<&str>) -> Result<ShellCommand> {
        match (&self.manifest().guest, &target.native) {
            (Some(guest), Some(Native::Compose { service })) => crate::in_vm::console(cx, &self.driver, guest, service.as_str(), user),
            _ => self.driver.console_command(cx, target, user),
        }
    }
}
