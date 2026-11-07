//! The ad-hoc attacker box of a container lab in a VM (`docker-vm`, the manifest's `guest`): a
//! Docker container the runner stands up inside the helper VM, on the lab's Docker network there,
//! with no Compose service and no manifest machine behind it — what the Compose driver's ad-hoc
//! box is on this computer's Docker, done through `vagrant ssh <vm> -c "docker …"` with the run's
//! state folder (`VAGRANT_DOTFILE_PATH`). An idle container from the learner's image (Kali by
//! default), named after the run, on the chosen lab network (the first otherwise) and plugged into
//! the rest, its address read back with `docker inspect`; `detach` removes it by name,
//! idempotently; status finds it by its deterministic name, as the Compose driver does.
//!
//! Docker in the helper VM is root's (Isoloom's `docker-vm` runs every Compose command there with
//! `sudo`), so every `docker` here is too.

use std::collections::BTreeMap;
use std::time::Duration;

use isoloom_manifest::{Native, access};
use serde_json::Value;

use super::{Vagrant, define_name};
use crate::Result;
use crate::driver::{ADHOC_NAME, ADHOC_STATUS_FORMAT, Attached, Driver, adhoc_status};
use crate::executor::{Cx, Echo, call_ok};
use crate::process::ProcessSpec;
use crate::ssh::sh_quote;
use crate::state::Op;
use crate::status::{MachineStatus, NetworkView, ShellCommand};

/// The label the ad-hoc box carries, so it is recognisable as one Isothrum stood up.
const LABEL: &str = "isoloom.access";

/// What the box runs: idle forever, so it stays up for the learner to `exec` into.
const IDLE: &str = "sleep infinity";

/// The shell the box's `exec` opens: `bash` when the image has it (Kali does), else `sh`.
const SHELL: &str = "command -v bash >/dev/null 2>&1 && exec bash || exec sh";

/// How long hot-adding the box may take: its image may still need pulling, inside the VM.
const ATTACH_TIMEOUT: Duration = Duration::from_secs(900);

/// How long one of the box's quick calls in the VM may take (a `docker rm`, an inspect): one
/// `vagrant ssh`, which on some providers reads the VM's SSH port from the hypervisor first.
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// A `docker inspect` of the box: the operation it reads for, its template, and how long it may
/// take.
struct Probe {
    op: Op,
    format: &'static str,
    limit: Duration,
}

/// The box's networks as JSON, read right after it came up.
const ADDRESS: Probe = Probe {
    op: Op::Attach,
    format: "{{json .NetworkSettings.Networks}}",
    limit: CALL_TIMEOUT,
};

/// The box's running flag and its networks, a `|` apart, for a status read (which must stay
/// quick).
const STATUS: Probe = Probe {
    op: Op::Check,
    format: ADHOC_STATUS_FORMAT,
    limit: Duration::from_secs(60),
};

/// The define name of the VM the lab's containers run in, when this run is a container lab in a
/// VM (the manifest's `guest` names one of its Vagrant machines); `None` for a VM lab.
pub(super) fn guest_vm(cx: &Cx<'_>) -> Option<String> {
    let manifest = cx.run.manifest();
    let guest = manifest.guest.as_ref()?;
    manifest
        .machines
        .iter()
        .find(|m| m.name == guest.machine && matches!(m.native, Some(Native::Vagrant { .. })))
        .map(|m| define_name(m).to_owned())
}

/// The deterministic name of the run's ad-hoc access box in the VM.
fn container_name(cx: &Cx<'_>) -> String {
    format!("{}-{ADHOC_NAME}", cx.run.name())
}

/// The lab's networks, their Docker names in the VM filled, `chosen` (else the first) at the
/// front: the box lands on that one and is plugged into the rest.
fn lab_networks(cx: &Cx<'_>, chosen: Option<&str>) -> Vec<NetworkView> {
    let mut networks = crate::status::networks(cx.run.manifest(), |t| cx.vars.render(t, "networks.native").ok());
    if let Some(at) = chosen.and_then(|c| networks.iter().position(|n| n.name == c)) {
        let first = networks.remove(at);
        networks.insert(0, first);
    }
    networks
}

/// `command` run in `vm` by the run's Vagrant (`vagrant ssh --no-tty --command <command> <vm>`),
/// for the step `id`.
fn in_vm(v: &Vagrant, cx: &Cx<'_>, id: &str, vm: &str, command: &str) -> Result<ProcessSpec> {
    v.vagrant(cx, id, &["ssh", "--no-tty", "--command", command, vm])
}

/// A network's non-empty `IPAddress`, as `docker inspect` writes it.
fn ip_of(net: &Value) -> Option<&str> {
    net.get("IPAddress").and_then(Value::as_str).filter(|a| !a.is_empty())
}

/// The box's addresses from the inspected networks JSON, keyed by lab network name (`networks`
/// maps each lab network to its Docker name); a network that isn't the lab's, or one with no
/// readable address, is dropped.
fn addresses_by_lab_network(json: &str, networks: &[NetworkView]) -> BTreeMap<String, String> {
    let Ok(Value::Object(nets)) = serde_json::from_str::<Value>(json) else {
        return BTreeMap::new();
    };
    networks
        .iter()
        .filter_map(|n| {
            let docker = n.native.as_deref()?;
            Some((n.name.clone(), ip_of(nets.get(docker)?)?.to_owned()))
        })
        .collect()
}

/// Stands the ad-hoc box up inside `vm` on the running lab: an idle container from `image` (Kali
/// by default), named after the run, on `network` (the lab's first otherwise) and plugged into
/// the lab's other networks best effort. Reports where it is.
pub(super) async fn attach(v: &Vagrant, cx: &Cx<'_>, vm: &str, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
    let id = Op::Attach.as_str();
    let name = container_name(cx);
    let networks = lab_networks(cx, network);
    let primary = networks.first();
    // Idempotent: a box a previous attach left goes first, so `run --name` is free.
    drop(remove(v, cx, vm, &name, Echo::Quiet).await);
    let on = primary
        .and_then(|n| n.native.as_deref())
        .map(|net| format!(" --network {}", sh_quote(net)))
        .unwrap_or_default();
    let run = format!(
        "sudo docker run -d --name {} --label {LABEL}=true{on} {} {IDLE}",
        sh_quote(&name),
        sh_quote(image.unwrap_or(access::DEFAULT_IMAGE))
    );
    call_ok(cx, id, &in_vm(v, cx, id, vm, &run)?, ATTACH_TIMEOUT, Echo::Shown).await?;
    for docker_net in networks.iter().skip(1).filter_map(|n| n.native.as_deref()) {
        // Already on it, or a network it can't join, is left as it is.
        let connect = format!("sudo docker network connect {} {}", sh_quote(docker_net), sh_quote(&name));
        if let Err(e) = call_ok(cx, id, &in_vm(v, cx, id, vm, &connect)?, CALL_TIMEOUT, Echo::Quiet).await {
            tracing::debug!(%e, "ad-hoc access box already on the network, or it can't be joined");
        }
    }
    let address = inspect(v, cx, vm, &name, &ADDRESS)
        .await
        .map(|json| addresses_by_lab_network(&json, &networks))
        .and_then(|found| primary.and_then(|n| found.get(&n.name).cloned()));
    Ok(Attached {
        name: Some(name),
        network: primary.map(|n| n.name.clone()),
        address,
    })
}

/// Removes the box from `vm` by name, best effort: a box already gone is no failure to report.
pub(super) async fn detach(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Result<()> {
    drop(remove(v, cx, vm, &container_name(cx), Echo::Shown).await);
    Ok(())
}

/// `docker rm -f <name>` in `vm`.
async fn remove(v: &Vagrant, cx: &Cx<'_>, vm: &str, name: &str, echo: Echo) -> Result<()> {
    let id = Op::Detach.as_str();
    let rm = format!("sudo docker rm -f {}", sh_quote(name));
    call_ok(cx, id, &in_vm(v, cx, id, vm, &rm)?, CALL_TIMEOUT, echo).await.map(drop)
}

/// `docker inspect -f <probe's format> <name>` in `vm`, quietly: its output, or `None` when it
/// can't be read (the box isn't there, the VM doesn't answer).
async fn inspect(v: &Vagrant, cx: &Cx<'_>, vm: &str, name: &str, probe: &Probe) -> Option<String> {
    let id = probe.op.as_str();
    let cmd = format!("sudo docker inspect -f {} {}", sh_quote(probe.format), sh_quote(name));
    call_ok(cx, id, &in_vm(v, cx, id, vm, &cmd).ok()?, probe.limit, Echo::Quiet).await.ok()
}

/// The ad-hoc box as a status machine, when one is up in `vm`: its running state and its address
/// on each lab network. `None` when there is no box (best effort: a box that isn't there, or an
/// inspect that can't be read, is simply no access machine). The box has no Compose service, so
/// the guest's `ps` never lists it; this finds it by its deterministic name.
pub(super) async fn status(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Option<MachineStatus> {
    let out = inspect(v, cx, vm, &container_name(cx), &STATUS).await?;
    adhoc_status(&out, |networks| addresses_by_lab_network(networks, &lab_networks(cx, None)))
}

/// The command that opens an interactive shell into the box: `vagrant ssh <vm> -c "sudo docker
/// exec -it <name> …"` (falling back from `bash` to `sh`), in the run's folder, with the run's
/// Vagrant environment (its state folder, its forwarded ports).
pub(super) fn shell_command(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Result<ShellCommand> {
    let exec = format!("sudo docker exec -it {} sh -c {}", sh_quote(&container_name(cx)), sh_quote(SHELL));
    Ok(ShellCommand {
        program: "vagrant".into(),
        args: vec!["ssh".into(), vm.to_owned(), "-c".into(), exec],
        cwd: Some(cx.run.dir().to_path_buf()),
        env: v.env(cx)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn network(name: &str, native: Option<&str>) -> NetworkView {
        NetworkView {
            name: name.to_owned(),
            cidr: "10.30.0.0/24".into(),
            internet: false,
            native: native.map(str::to_owned),
        }
    }

    #[test]
    fn addresses_are_keyed_by_lab_network_through_their_docker_names() {
        let json = r#"{"flag-portal_lab":{"IPAddress":"10.30.0.77"},"flag-portal_dmz":{"IPAddress":"10.40.0.5"},"bridge":{"IPAddress":"172.17.0.2"},"flag-portal_down":{"IPAddress":""}}"#;
        let networks = [
            network("lab", Some("flag-portal_lab")),
            network("dmz", Some("flag-portal_dmz")),
            network("down", Some("flag-portal_down")),
            network("unnamed", None),
        ];
        let got = addresses_by_lab_network(json, &networks);
        assert_eq!(got.get("lab").map(String::as_str), Some("10.30.0.77"));
        assert_eq!(got.get("dmz").map(String::as_str), Some("10.40.0.5"));
        assert!(!got.contains_key("down"), "a network with no address is dropped");
        assert!(!got.contains_key("unnamed"), "a network with no Docker name can't be matched");
        assert_eq!(got.len(), 2, "a network outside the lab is dropped");
    }

    #[test]
    fn an_unreadable_inspect_yields_no_addresses() {
        let networks = [network("lab", Some("flag-portal_lab"))];
        assert!(addresses_by_lab_network("not json", &networks).is_empty());
        assert!(addresses_by_lab_network("[]", &networks).is_empty());
        assert!(addresses_by_lab_network("{}", &networks).is_empty());
    }
}
