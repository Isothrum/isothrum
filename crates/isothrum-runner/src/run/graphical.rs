//! A machine's graphical console: where it answers, found where the manifest says, reached on
//! this computer's loopback (an SSH tunnel opened when it must be), for the embedder's viewer.

use super::Running;
use crate::Result;
use crate::graphical::{Console, none, offered, reach};

impl Running<'_> {
    /// `machine`'s graphical console (its screen), as its manifest says it has one under the
    /// run's provider (`machines[].consoles`): its protocol (VNC or RDP), where it answers
    /// ([`crate::Endpoint`]: a port of this computer's loopback, or Proxmox's VNC websocket), the
    /// account and password that sign in. A console the hypervisor says listens anywhere but the
    /// loopback is refused, and so is one on the loopback that doesn't answer. A console reached
    /// through an SSH tunnel holds it open: drop it once the viewer is done. Emits nothing, never
    /// locks the run, and writes no state but a Proxmox request's credentials while `curl` reads
    /// them. While an operation holds the run, the machine is as the runner last read it: the
    /// hypervisor is asked where its console is all the same.
    ///
    /// # Errors
    ///
    /// `machine_unknown` for a machine the run doesn't have; `no_console` when it has none there
    /// (the error says why, in the manifest's words: UTM's SPICE, VirtualBox without its
    /// extension pack…), isn't running, another process acts on it (Vagrant's lock: its start or
    /// its provisioning), its hypervisor doesn't say where its console is or started it without
    /// one, it doesn't answer there, or the runner chose none for it yet (VMware's: the run's
    /// next start chooses it); a tool's own failure (`tool_missing` for a missing `virsh`,
    /// `VBoxManage`, `curl` or `ssh`).
    pub async fn graphical_console(&self, machine: &str) -> Result<Console> {
        let target = self.machine_named(machine)?;
        let events = self.silent();
        let (record, mut cx) = self.silent_cx(&events)?;
        let console = offered(target, cx.scope.provider)?;
        let (_, observed, _) = self.read_for_query(&mut cx, record.as_ref()).await?;
        runs_now(target, &observed)?;
        super::made(&cx)?;
        self.driver.prepare(&cx)?;
        if cx.relayed.is_none() {
            self.relay(&mut cx).await?;
        }
        let outputs = self.bind_observed(&mut cx, observed).values;
        reach(&cx, target, console, &outputs).await
    }
}

/// Refuses `target` unless `observed` has it running: busy (another process acts on it), or
/// stopped, absent…, saying so.
fn runs_now(target: &isoloom_manifest::Machine, observed: &crate::driver::Observed) -> Result<()> {
    let state = observed.machines.iter().find(|m| m.name == target.name.as_str()).map(|m| m.state.clone());
    match state {
        Some(crate::MachineState::Busy) => Err(none(
            target,
            "another process is acting on it (starting or provisioning it): its screen can be shown once that is done",
        )),
        Some(s) if s.runs() => Ok(()),
        other => {
            let said = other.map_or_else(|| "absent".to_owned(), |s| s.to_string());
            Err(none(target, format!("it isn't running ({said})")))
        }
    }
}
