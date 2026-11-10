//! Compose inside one of the run's VMs (`docker-vm`, the manifest's `guest`): the VM is its
//! driver's (Vagrant, Terraform), the containers Compose's, read inside the VM through the
//! manifest's `guest.status` (`docker compose ps --all --format json`, through `vagrant ssh` or
//! `ssh.run`). Their published ports answer where the VM does: this computer's loopback where the
//! VM forwards them, the VM's own address elsewhere (`guest.host`, else the driver's host for the
//! VM). While the VM isn't running, its containers stand as it does. A read that fails while the
//! VM runs is tried once more after a moment: the way in can miss once (an SSH port read from the
//! hypervisor that answers late, a session refused while the VM settles), and one miss must not
//! make a run that is up read as not running, or refuse an operation on it.
//!
//! A console into one of those containers, and its removal at a retire, run a script inside the
//! VM: the VM's driver says only how a script runs there ([`AnyDriver::in_guest`]).

use std::collections::BTreeMap;
use std::time::Duration;

use isoloom_manifest::{Guest, Machine, Native};

use crate::Result;
use crate::console::ShellCommand;
use crate::driver::{AnyDriver, COMPOSE_SERVICE_LABEL, GuestUse, Observed, REMOVE_LIMIT, console_in_run, shells};
use crate::error::Error;
use crate::executor::{Cx, Echo, call_ok, capture_step};
use crate::ssh::sh_quote;
use crate::status::{MachineState, MachineStatus};
use crate::vars::PortKey;

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
        Some(t) => cx
            .vars
            .render(t, "guest.host")
            .inspect_err(|e| tracing::warn!(error = %e, "the guest's host couldn't be filled: `${{host.<machine>}}` stays unset"))
            .ok(),
        None => observed.hosts.get(guest.machine.as_str()).cloned(),
    };
    let read = if vm == MachineState::Running {
        read_containers(cx, guest).await
    } else {
        Inside::VmDown
    };
    let manifest = cx.run.manifest();
    let (inside, published) = match &read {
        Inside::Read(inside, published) => (inside.as_slice(), published.clone()),
        Inside::Unreadable(_) | Inside::VmDown => (&[][..], BTreeMap::new()),
    };
    for m in observed.machines.iter_mut().filter(|m| is_container_in_vm(manifest, &m.name)) {
        *m = match (&read, inside.iter().find(|x| x.name == m.name)) {
            (Inside::Read(..), Some(x)) => x.clone(),
            (Inside::Read(..), None) => standing(m, MachineState::Absent),
            // The VM runs but its containers can't be read yet (SSH not answering), or what was
            // read lists none this runner can read.
            (Inside::Unreadable(why), _) => standing(m, MachineState::Unknown(format!("unreadable: {why}"))),
            // Nothing runs in a VM that doesn't: each container stands as the VM does.
            (Inside::VmDown, _) => standing(m, vm.clone()),
        };
    }
    for ((machine, port), host_port) in published {
        if let Some(host) = &host {
            observed.hosts.insert(machine.clone(), host.clone());
        }
        observed.published.insert((machine, port), host_port);
    }
}

/// What was read of the containers in the run's VM.
enum Inside {
    /// The containers `guest.status` listed there, and the host port each published machine port
    /// got.
    Read(Vec<MachineStatus>, BTreeMap<PortKey, u16>),
    /// The VM runs but its containers couldn't be read: the last failure's words.
    Unreadable(String),
    /// The VM doesn't run, so nothing was read.
    VmDown,
}

/// The containers as `guest.status` lists them in the running VM, read up to [`READS`] times,
/// [`RETRY`] apart (a read whose output lists no container this runner can read counts as
/// failed); unreadable, with the last failure's words, otherwise.
async fn read_containers(cx: &Cx<'_>, guest: &Guest) -> Inside {
    let mut failed = String::new();
    for attempt in 0..READS {
        if attempt > 0 {
            tokio::time::sleep(RETRY).await;
        }
        match capture_step(cx, "guest.status", &guest.status).await {
            Ok(out) => match crate::driver::read_ps(cx.run.manifest(), &out) {
                Some((inside, published)) => return Inside::Read(inside, published),
                None => crate::driver::UNREADABLE_PS.clone_into(&mut failed),
            },
            Err(e) => failed = e.to_string(),
        }
    }
    Inside::Unreadable(failed)
}

/// Whether the machine `name` is one of the containers Compose runs in the VM.
fn is_container_in_vm(manifest: &isoloom_manifest::Manifest, name: &str) -> bool {
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

/// The command that opens a console into Compose service `service`, running in `guest`'s VM:
/// into the VM, then into the service's container there ([`into_service`]).
pub(crate) fn console(cx: &Cx<'_>, driver: &AnyDriver, guest: &Guest, service: &str) -> Result<ShellCommand> {
    let spec = driver.in_guest(cx, vm_of(cx, guest)?, &into_service(service), GuestUse::Console)?;
    Ok(console_in_run(cx, spec))
}

/// Removes Compose service `service`, running in `guest`'s VM, there with the containers in its
/// namespace ([`remove_service`]); nothing of the VM goes.
pub(crate) async fn retire(cx: &Cx<'_>, driver: &AnyDriver, guest: &Guest, service: &str) -> Result<()> {
    let spec = driver.in_guest(cx, vm_of(cx, guest)?, &remove_service(service), GuestUse::Step)?;
    call_ok(cx, cx.events.call_id(), &spec, REMOVE_LIMIT, Echo::Shown).await.map(drop)
}

/// The VM `guest` names, among the manifest's machines.
fn vm_of<'m>(cx: &Cx<'m>, guest: &Guest) -> Result<&'m Machine> {
    cx.run
        .manifest()
        .machines
        .iter()
        .find(|m| m.name == guest.machine)
        .ok_or_else(|| Error::Unsupported {
            what: format!("the VM `{}` the run's containers run in, which the manifest doesn't have", guest.machine),
        })
}

/// Run inside the VM: a shell in the container of Compose service `service` there, found by its
/// Compose label (whatever project and file started it).
fn into_service(service: &str) -> String {
    let filter = sh_quote(&format!("label={COMPOSE_SERVICE_LABEL}={service}"));
    format!(
        "sudo docker exec -it \"$(sudo docker ps -q --filter {filter} | head -n 1)\" sh -c {}",
        sh_quote(shells::CONTAINER_SHELL)
    )
}

/// Run inside the VM: Compose service `service`'s container removed, with every container in its
/// network namespace (`network_mode: service:<service>`: part of the same machine), found by its
/// Compose label as [`into_service`] finds it. Nothing to do when it isn't there.
fn remove_service(service: &str) -> String {
    let filter = sh_quote(&format!("label={COMPOSE_SERVICE_LABEL}={service}"));
    format!(
        "set -e; id=$(sudo docker ps -aq --no-trunc --filter {filter} | head -n 1); [ -n \"$id\" ] || exit 0; \
for c in $(sudo docker ps -aq --no-trunc); do \
if [ \"$(sudo docker inspect -f '{{{{.HostConfig.NetworkMode}}}}' \"$c\")\" = \"container:$id\" ]; then sudo docker rm -f \"$c\" >/dev/null; fi; done; \
sudo docker rm -f \"$id\" >/dev/null"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_in_a_vm_is_entered_by_its_compose_label() {
        assert_eq!(
            into_service("attackbox"),
            "sudo docker exec -it \"$(sudo docker ps -q --filter 'label=com.docker.compose.service=attackbox' | head -n 1)\" sh -c 'if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi'"
        );
    }

    #[test]
    fn a_service_in_a_vm_is_removed_with_what_shares_its_namespace() {
        let script = remove_service("user");
        assert!(script.contains("--filter 'label=com.docker.compose.service=user'"), "{script}");
        assert!(script.contains("= \"container:$id\" ]; then sudo docker rm -f"), "its sidecars: {script}");
        assert!(script.ends_with("sudo docker rm -f \"$id\" >/dev/null"), "{script}");
        assert!(script.contains("[ -n \"$id\" ] || exit 0"), "nothing there: nothing to do");
    }
}
