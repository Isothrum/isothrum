//! A machine's graphical console, found where its hypervisor keeps it through the run's driver,
//! reached on this computer's loopback (or Proxmox's API), for the embedder's viewer; and what
//! opens it in the hypervisor's own app.

use std::time::Duration;

use super::Running;
use crate::Result;
use crate::events::Emitter;
use crate::formats::{Seen, ToolCx};
use crate::graphical::{Console, ConsoleOffer, Endpoint, NativeConsole, no_console, on_loopback};
use crate::options::RunOptions;
use crate::status::MachineState;

/// How long a console on the loopback gets to accept a connection before it is said not to
/// answer.
const ANSWER_LIMIT: Duration = Duration::from_secs(2);

impl Running<'_> {
    /// What a status says of each machine `seen` has, in its order: a console its driver can't
    /// say anything of is none, traced.
    pub(super) async fn screens(&self, cx: &ToolCx<'_>, seen: &Seen) -> Vec<Option<ConsoleOffer>> {
        let mut out = Vec::with_capacity(seen.machines.len());
        for m in &seen.machines {
            out.push(self.driver.graphical(cx, seen, m).await.unwrap_or_else(|e| {
                tracing::debug!(machine = %m.name, error = %e, "the machine's console couldn't be said");
                None
            }));
        }
        out
    }

    /// `machine`'s graphical console (its screen), where its hypervisor keeps it: its protocol
    /// (VNC or RDP), where it answers ([`crate::Endpoint`]: a port of this computer's loopback, or
    /// Proxmox's VNC websocket) and the account and password that sign in. A console that
    /// answers only on the run's own networks (a module's Windows VM's Remote Desktop) is reached
    /// through an SSH tunnel to the machine beside it the driver names: `ssh -N -L
    /// 127.0.0.1:<free port>:<its address>:<its port>`, signed in with the run's key (its
    /// `ssh_private_key_file`, else the runner's) and checked against the host key the run
    /// pinned the first time it met that machine. The returned [`Console`] owns the tunnel
    /// ([`Console::tunnel`]): keep it while the viewer shows the screen; dropping it ends the
    /// tunnel, and so does a `down` of the run. A console the hypervisor says
    /// listens anywhere but the loopback is refused, and so is one on the loopback that doesn't
    /// answer. `opts` gives what the tools get (a Proxmox server's address and credentials, as
    /// its provider reads them). Emits nothing and never locks the run; while an operation holds
    /// it, the hypervisor is asked all the same.
    ///
    /// # Errors
    ///
    /// `machine_unknown` for a machine the run doesn't have; `no_console` when it has none (the
    /// error says why: a hypervisor whose screen opens only in its own app, VirtualBox without
    /// its extension pack…), isn't running, its hypervisor doesn't say where its console is or
    /// started it without one, or it doesn't answer there; `server_auth_failed` when Proxmox
    /// refuses the run's credentials; a tool's own failure (`tool_missing` for a missing `virsh`,
    /// `VBoxManage`, `curl`, `ssh` or `ssh-keyscan`); `tool_failed` when the machine a tunnel goes
    /// through gives no SSH host key to pin.
    pub async fn graphical_console(&self, machine: &str, opts: &RunOptions) -> Result<Console> {
        let mut console = self
            .query(opts, async |cx: &ToolCx<'_>| {
                self.running_now(cx, machine).await?;
                self.driver.reach_graphical(cx, machine).await
            })
            .await?;
        if let Some(to) = console.through.take() {
            let tunnel = crate::graphical::tunnel::open(self.runner, &self.name, &self.state_dir(), machine, &to).await?;
            console.endpoint = Endpoint::Loopback(tunnel.local());
            console.tunnel = Some(tunnel);
        }
        if let Endpoint::Loopback(at) = &console.endpoint {
            let at = on_loopback(machine, *at)?;
            if !self.runner.config.ports.reachable(&at.ip().to_string(), at.port(), ANSWER_LIMIT).await {
                return Err(no_console(machine, format!("its screen didn't answer at {at}")));
            }
        }
        Ok(console)
    }

    /// What opens running VM `machine` in its hypervisor's own app, beside the embedder's viewer
    /// ([`crate::Status::native`] names the app): a program and its arguments, a macOS app and
    /// what it opens, or an `https` URL, best first, each argument checked; the embedder runs the
    /// first this computer has, never through a shell. Reads as [`Running::graphical_console`]
    /// does, and runs nothing but a read (Parallels' `prlctl`, UTM's `utmctl`, libvirt's `virsh`).
    ///
    /// # Errors
    ///
    /// `machine_unknown` for a machine the run doesn't have; `no_console` when its hypervisor has
    /// no app (QEMU, ESXi, a container), it isn't running, or what names it can't be handed to
    /// the app as it is.
    pub async fn native_console(&self, machine: &str, opts: &RunOptions) -> Result<NativeConsole> {
        self.query(opts, async |cx: &ToolCx<'_>| {
            self.running_now(cx, machine).await?;
            self.driver.native_console(cx, machine).await
        })
        .await
    }

    /// Runs `work` with a silent tool context of this run (nothing emitted, nothing locked).
    async fn query<T>(&self, opts: &RunOptions, work: impl AsyncFnOnce(&ToolCx<'_>) -> Result<T>) -> Result<T> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        self.with_cx(opts, &emitter, self.runner.config.processes.as_ref(), work).await
    }

    /// Refuses `machine` unless the run has it running, as its tool says now; while an operation
    /// holds the run its tool isn't asked (it would contend with it), and the hypervisor answers
    /// for itself.
    async fn running_now(&self, cx: &ToolCx<'_>, machine: &str) -> Result<()> {
        let recorded = self.runner.store.record(&self.name)?.map(|r| r.state);
        if self.runner.op_of(&self.name).is_some() || recorded.is_some_and(crate::state::RunState::is_moving) {
            return Ok(());
        }
        let (seen, _) = self.driver.observe(cx).await?;
        let Some(found) = seen.machines.iter().find(|m| m.name == machine) else {
            return Err(crate::error::Error::MachineUnknown {
                run: self.name.to_string(),
                machine: machine.to_owned(),
            });
        };
        match &found.state {
            MachineState::Busy => Err(no_console(
                machine,
                "another process is acting on it (starting or provisioning it): its screen can be shown once that is done",
            )),
            s if s.runs() => Ok(()),
            s => Err(no_console(machine, format!("it isn't running ({s})"))),
        }
    }
}
