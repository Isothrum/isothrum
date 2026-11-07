//! Compose inside one of the run's VMs (`docker-vm`, the manifest's `guest`): the VM is its
//! driver's (Vagrant, Terraform), the containers Compose's, read inside the VM through the
//! manifest's `guest.status` (`docker compose ps --all --format json`, through `vagrant ssh` or
//! `ssh.run`). Their published ports answer where the VM does: this computer's loopback where the
//! VM forwards them, the VM's own address elsewhere (`guest.host`, else the driver's host for the
//! VM). While the VM isn't running, its containers stand as it does. A read that fails while the
//! VM runs is tried once more after a moment: the way in can miss once (an SSH port read from the
//! hypervisor that answers late, a session refused while the VM settles), and one miss must not
//! make a running lab read as not running, or refuse an operation on it.

use std::time::Duration;

use isoloom_manifest::{Guest, Native};

use crate::driver::Observed;
use crate::executor::{Cx, capture_step};
use crate::status::{MachineState, MachineStatus};

/// How many times the containers are read while the VM runs, before the read counts as failed.
const READS: u32 = 2;

/// How long between two reads.
const RETRY: Duration = Duration::from_secs(1);

/// `observed` (the VM's driver's) with the containers as `guest` reads them in the VM. The
/// outputs the driver read are bound first: the VM's address may be one (`${outputs.ip}`).
pub(crate) async fn observe(cx: &mut Cx<'_>, guest: &Guest, observed: &mut Observed) {
    for (name, value) in &observed.outputs {
        cx.vars.set_output(name, value.clone());
    }
    let vm = observed
        .machines
        .iter()
        .find(|m| m.name == guest.machine.as_str())
        .map_or(MachineState::Absent, |m| m.state.clone());
    let host = match &guest.host {
        Some(t) => cx.vars.render(t, "guest.host").ok(),
        None => observed.hosts.get(guest.machine.as_str()).cloned(),
    };
    let read = if vm == MachineState::Running {
        read_containers(cx, guest).await
    } else {
        Err(String::new())
    };
    let manifest = cx.run.manifest();
    let (inside, published) = match &read {
        Ok(out) => crate::driver::read_ps(manifest, out),
        Err(_) => (Vec::new(), std::collections::BTreeMap::new()),
    };
    for m in observed.machines.iter_mut().filter(|m| in_guest(manifest, &m.name)) {
        *m = match (&read, inside.iter().find(|x| x.name == m.name)) {
            (Ok(_), Some(x)) => x.clone(),
            (Ok(_), None) => standing(m, MachineState::Absent),
            // The VM runs but its containers can't be read yet (SSH not answering).
            (Err(why), _) if vm == MachineState::Running => standing(m, MachineState::Unknown(format!("unreadable: {why}"))),
            (Err(_), _) => standing(m, vm.clone()),
        };
    }
    for ((machine, port), host_port) in published {
        if let Some(host) = &host {
            observed.hosts.insert(machine.clone(), host.clone());
        }
        observed.published.insert((machine, port), host_port);
    }
}

/// The containers as `guest.status` lists them in the running VM, read up to [`READS`] times,
/// [`RETRY`] apart; the last failure's words otherwise.
async fn read_containers(cx: &Cx<'_>, guest: &Guest) -> Result<String, String> {
    let mut failed = String::new();
    for attempt in 0..READS {
        if attempt > 0 {
            tokio::time::sleep(RETRY).await;
        }
        match capture_step(cx, "guest.status", &guest.status).await {
            Ok(out) => return Ok(out),
            Err(e) => failed = e.to_string(),
        }
    }
    Err(failed)
}

/// Whether the machine `name` is one of the containers Compose runs in the VM.
fn in_guest(manifest: &isoloom_manifest::Manifest, name: &str) -> bool {
    manifest
        .machines
        .iter()
        .any(|m| m.name.as_str() == name && matches!(m.native, Some(Native::Compose { .. })))
}

/// `m` in `state`, with nothing published.
fn standing(m: &MachineStatus, state: MachineState) -> MachineStatus {
    MachineStatus {
        state,
        published: Vec::new(),
        ..m.clone()
    }
}
