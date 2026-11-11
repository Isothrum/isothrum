//! A machine's graphical console: where it answers, found where the manifest says, reached on
//! this computer's loopback (an SSH tunnel opened when it must be), for the embedder's viewer.

use super::Running;
use crate::Result;
use crate::graphical::{Console, offered, reach};

impl Running<'_> {
    /// `machine`'s graphical console (its screen), as its manifest says it has one under the
    /// run's provider (`machines[].consoles`): its protocol (VNC or RDP), where it answers
    /// ([`crate::Endpoint`]: a port of this computer's loopback, or Proxmox's VNC websocket), the
    /// account and password that sign in. A console the hypervisor says listens anywhere but the
    /// loopback is refused. A console reached through an SSH tunnel holds it open: drop it once
    /// the viewer is done. Emits nothing; never locks the run.
    ///
    /// # Errors
    ///
    /// `machine_unknown` for a machine the run doesn't have; `no_console` when it has none there
    /// (the error says why, in the manifest's words: UTM's SPICE, VirtualBox without its
    /// extension pack…), isn't running, or its hypervisor doesn't say where its console is; a
    /// tool's own failure (`tool_missing` for a missing `virsh`, `VBoxManage`, `curl` or `ssh`).
    pub async fn graphical_console(&self, machine: &str) -> Result<Console> {
        let target = self.machine_named(machine)?;
        let events = self.silent();
        let (_, mut cx) = self.silent_cx(&events)?;
        let console = offered(target, cx.scope.provider)?;
        super::made(&cx)?;
        self.driver.prepare(&cx)?;
        self.relay(&mut cx).await?;
        let observed = self.observe(&mut cx).await?;
        let state = observed.machines.iter().find(|m| m.name == machine).map(|m| m.state.clone());
        if !state.as_ref().is_some_and(crate::MachineState::runs) {
            let state = state.map_or_else(|| "absent".to_owned(), |s| s.to_string());
            return Err(crate::graphical::none(target, format!("it isn't running ({state})")));
        }
        let outputs = self.bind_observed(&mut cx, observed).values;
        reach(&cx, target, console, &outputs).await
    }
}
