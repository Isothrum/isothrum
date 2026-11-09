//! The way into a machine of a run: the command that opens a console into it, built by the
//! runner (it alone knows the runtime) and run by the embedder in its own terminal.

use isoloom_manifest::{Machine, Native};

use super::Running;
use crate::Result;
use crate::executor::Cx;
use crate::status::ShellCommand;

impl Running<'_> {
    /// The command that opens an interactive console into `machine`, any machine of the run (an
    /// SSH login on a cloud's), by its native name (the Compose service, the Vagrant define), so
    /// the embedder never reconstructs a container name. A pure query: it runs nothing, emits
    /// nothing, and doesn't check the machine is up (the embedder runs what it returns in its own
    /// terminal once the machine is up or attached).
    ///
    /// # Errors
    ///
    /// Fails with `machine_unknown` for a machine the run doesn't have, `not_running` when the
    /// run gives it no address yet, and `unsupported` where the target has no way in yet.
    pub fn console_command(&self, machine: &str) -> Result<ShellCommand> {
        let target = self.machine_named(machine)?;
        self.query(|cx| self.console_of(cx, target))
    }

    /// The console into `target`: into the run's VM, then into its container, for a Compose
    /// service in that VM ([`crate::in_vm::console`]); the driver's otherwise.
    fn console_of(&self, cx: &Cx<'_>, target: &Machine) -> Result<ShellCommand> {
        match (&self.manifest().guest, &target.native) {
            (Some(guest), Some(Native::Compose { service })) => crate::in_vm::console(cx, &self.driver, guest, service.as_str()),
            _ => self.driver.console_command(cx, target),
        }
    }
}
