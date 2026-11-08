//! The learner's attack box on a Terraform run with no container host VM (a lab run as one VM
//! per machine in a cloud account): an instance of its own, which Isoloom's module makes while
//! its `attack_box` variable names the box's image. The module declares that variable as a param
//! ([`PARAM`]); a run whose manifest has it takes its box this way.
//!
//! - **Attach:** the module applied again with [`PARAM`] set (the run's [`crate::Running::attach`]
//!   sets it), in the `kali` form first: the instance is Kali itself, nothing wrapped. Only when
//!   the account can't run Kali's image (its Marketplace terms not accepted) is it applied in the
//!   `container` form: a Debian instance running the box's image. Then the box is waited for over
//!   SSH: cloud-init writes [`READY`] once it is set up, [`FAILED`] with why when it couldn't be.
//! - **Detach:** applied again with [`PARAM`] cleared: the instance goes.
//! - **Status:** the box is up while the state's outputs give its address.
//! - **Shell:** `ssh -t` to the instance: a login shell on the Kali box, or `docker exec` into the
//!   container of the `container` form.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::time::Duration;

use super::{LOCK_FILE, remove, state};
use crate::Result;
use crate::driver::{ADHOC_NAME, Attached, vm_box};
use crate::error::Error;
use crate::executor::{Cx, Echo, call, call_ok, spec_for};

use crate::ssh::{self, Login};
use crate::status::{MachineState, MachineStatus, ShellCommand};

/// The param (and the module's variable) naming the box's image; empty: no box.
pub(crate) const PARAM: &str = "attack_box";

/// The Terraform steps of `up` an attach or a detach runs again: the module applied.
const APPLY_STEPS: [&str; 2] = ["init", "apply"];

/// The module's variable choosing the box's form, its values, and the output saying which one
/// the box has.
const FORM_VAR: &str = "TF_VAR_attack_box_form";
const KALI: &str = "kali";
const CONTAINER_FORM: &str = "container";
const FORM_OUTPUT: &str = "attack_box_form";

/// What AWS says when the account hasn't accepted a Marketplace image's terms.
const NOT_SUBSCRIBED: [&str; 2] = ["OptInRequired", "aws-marketplace"];

/// The outputs the module gives while the box exists: its public address (where SSH reaches
/// it), its user, and its address on the lab's network.
const ADDRESS: &str = "address_isoloom_access";
const USER: &str = "ssh_user_isoloom_access";
const LAB_ADDRESS: &str = "lab_address_isoloom_access";

/// The name of the module's resources for the box (its instance, its security group).
const RESOURCE: &str = "isoloom_access";

/// The box's container in the instance, and the files cloud-init writes about it.
const CONTAINER: &str = "isoloom-access";
const READY: &str = "/var/lib/isoloom/attack-box-ready";
const FAILED: &str = "/var/lib/isoloom/attack-box-failed";

/// How long the box may take to come up after the apply (Kali's toolset installed, or Docker and
/// the image pulled).
const PATIENCE: Duration = Duration::from_secs(1800);
/// How long an apply step may take when the manifest doesn't say.
const STEP_LIMIT: Duration = Duration::from_secs(3600);
/// How long between two looks.
const RETRY: Duration = Duration::from_secs(10);
/// How long one look may take.
const LOOK_LIMIT: Duration = Duration::from_secs(60);

/// Whether `manifest`'s run takes its box as an instance of its own: no container host VM to put
/// it in, and the module offers one.
pub(crate) fn offered(manifest: &isoloom_manifest::Manifest) -> bool {
    manifest.guest.is_none() && manifest.params.iter().any(|p| p.name.as_str() == PARAM)
}

/// Adds the box: the module applied with the image set, then the box waited for.
pub(super) async fn attach(cx: &Cx<'_>) -> Result<Attached> {
    match apply(cx, KALI).await {
        Err(Error::ToolFailed { reason, .. }) if NOT_SUBSCRIBED.iter().any(|m| reason.contains(m)) => {
            cx.events.warning(
                crate::WarningCode::Fallback,
                "This AWS account can't run Kali's own image (accept its terms on the AWS Marketplace to get it): the attack box runs as a container instead.",
            );
            apply(cx, CONTAINER_FORM).await?;
        }
        done => done?,
    }
    let outputs = outputs(cx)?;
    let login = login(cx, &outputs)?;
    wait_ready(cx, &login).await?;
    let address = outputs.get(LAB_ADDRESS).filter(|a| !a.is_empty()).cloned();
    Ok(Attached {
        name: Some(ADHOC_NAME.to_owned()),
        network: address.as_deref().and_then(|a| network_of(cx.run.manifest(), a)),
        address,
    })
}

/// Takes the box out: the module applied with the image cleared. A run with none of the box's
/// resources is left as it is; one an attach left half made (its security group, after the
/// instance was refused) is cleared too.
pub(super) async fn detach(cx: &Cx<'_>) -> Result<()> {
    if !state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.names.iter().any(|n| n == RESOURCE) {
        return Ok(());
    }
    apply(cx, KALI).await
}

/// The box as status shows it, while the state gives its address.
pub(super) fn status(manifest: &isoloom_manifest::Manifest, outputs: &BTreeMap<String, String>) -> Option<MachineStatus> {
    let lab = outputs.get(LAB_ADDRESS).filter(|a| !a.is_empty())?;
    let addresses = network_of(manifest, lab).map(|n| (n, lab.clone())).into_iter().collect();
    Some(MachineStatus {
        name: ADHOC_NAME.to_owned(),
        role: isoloom_manifest::Role::Access,
        state: MachineState::Running,
        addresses,
        published: Vec::new(),
    })
}

/// `ssh -t` to the box's instance, then `docker exec` into the box.
pub(super) fn shell_command(cx: &Cx<'_>) -> Result<ShellCommand> {
    let outputs = outputs(cx)?;
    let login = login(cx, &outputs)?;
    // A Kali box is the instance itself; any other (the container form, or a module from before
    // the forms, which only had it) is a container in the instance.
    let exec = if outputs.get(FORM_OUTPUT).map(String::as_str) == Some(KALI) {
        "exec bash -l".to_owned()
    } else {
        format!("sudo docker exec -it {CONTAINER} sh -c {}", ssh::sh_quote(vm_box::SHELL))
    };
    let spec = ssh::spec(cx, &login, &exec, Vec::new())?;
    let mut args = vec!["-t".to_owned()];
    args.extend(spec.args);
    Ok(ShellCommand {
        program: spec.program,
        args,
        cwd: Some(cx.run.dir().to_path_buf()),
        env: Vec::new(),
    })
}

/// `up`'s Terraform steps again ([`APPLY_STEPS`]), with this operation's params and the box in
/// `form`: the module applied as the box's image says. A lock an interrupted apply left is
/// cleared first.
async fn apply(cx: &Cx<'_>, form: &str) -> Result<()> {
    remove(&cx.run.runner.store.run_dir(cx.run.name())?.join(LOCK_FILE))?;
    for id in APPLY_STEPS {
        let step = cx
            .run
            .manifest()
            .lifecycle
            .up
            .iter()
            .find(|s| s.id.as_str() == id)
            .ok_or_else(|| Error::Unsupported {
                what: format!("the attack box on a run whose `up` has no `{id}` step"),
            })?;
        let mut spec = spec_for(cx, "lifecycle.up", step)?;
        spec.env.push((FORM_VAR.to_owned(), form.to_owned()));
        let limit = step.timeout_s.map_or(STEP_LIMIT, |s| Duration::from_secs(u64::from(s.get())));
        call_ok(cx, id, &spec, limit, Echo::Shown).await?;
    }
    Ok(())
}

/// The run's Terraform outputs, read from its state.
fn outputs(cx: &Cx<'_>) -> Result<BTreeMap<String, String>> {
    Ok(state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs)
}

/// The SSH login of the box's instance; the run isn't running while it has no address.
fn login(cx: &Cx<'_>, outputs: &BTreeMap<String, String>) -> Result<Login> {
    let host = outputs.get(ADDRESS).filter(|a| !a.is_empty()).ok_or_else(|| Error::NotRunning {
        run: cx.run.name().to_string(),
    })?;
    let user = ssh::user_or_root(outputs.get(USER).cloned().unwrap_or_default());
    Login::new(&user, host, cx.ssh_jump.as_deref(), "")
}

/// Waits for cloud-init's word on the box: [`READY`], or [`FAILED`] (an error with its log).
/// SSH not answering yet (the instance booting) and no word yet are waited out, for [`PATIENCE`].
async fn wait_ready(cx: &Cx<'_>, login: &Login) -> Result<()> {
    let look = format!("if [ -f {READY} ]; then echo ready; elif [ -f {FAILED} ]; then cat {FAILED}; exit 3; fi");
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let spec = ssh::spec(cx, login, &look, Vec::new())?;
        let out = call(cx, "attach", &spec, LOOK_LIMIT, Echo::Quiet).await?;
        match out.exit.and_then(|e| e.code) {
            Some(0) if out.stdout.iter().any(|l| l.trim() == "ready") => return Ok(()),
            Some(3) => {
                return Err(Error::ToolFailed {
                    step: "attach".to_owned(),
                    reason: cx.events.mask(&format!("the attack box didn't start: {}", crate::executor::why_failed(&out))),
                });
            }
            _ if tokio::time::Instant::now() >= deadline => {
                return Err(Error::Timeout {
                    step: "attach".to_owned(),
                    after_s: PATIENCE.as_secs(),
                });
            }
            _ => tokio::time::sleep(RETRY).await,
        }
    }
}

/// The manifest's network whose block holds IPv4 `address`.
fn network_of(manifest: &isoloom_manifest::Manifest, address: &str) -> Option<String> {
    let ip: Ipv4Addr = address.parse().ok()?;
    manifest.networks.iter().find(|n| holds(&n.cidr.to_string(), ip)).map(|n| n.name.to_string())
}

/// Whether IPv4 block `cidr` (`10.20.0.0/24`) holds `ip`.
fn holds(cidr: &str, ip: Ipv4Addr) -> bool {
    let Some((base, len)) = cidr.split_once('/') else { return false };
    let (Ok(base), Ok(len)) = (base.parse::<Ipv4Addr>(), len.parse::<u32>()) else {
        return false;
    };
    let mask = u32::MAX.checked_shl(32 - len.min(32)).unwrap_or(0);
    u32::from(base) & mask == u32::from(ip) & mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_is_found_in_its_networks_block() {
        let ip = |s: &str| s.parse::<Ipv4Addr>().unwrap();
        assert!(holds("10.20.0.0/24", ip("10.20.0.57")));
        assert!(!holds("10.20.0.0/24", ip("10.20.1.5")));
        assert!(holds("0.0.0.0/0", ip("8.8.8.8")));
        assert!(!holds("nonsense", ip("10.0.0.1")));
    }
}
