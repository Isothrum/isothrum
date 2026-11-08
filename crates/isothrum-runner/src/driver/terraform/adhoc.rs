//! The ad-hoc attacker box of a container lab whose containers run in a VM Terraform made (the
//! manifest's `guest`: `cloud-docker` on every cloud, a container lab on a Proxmox server): the
//! shared in-VM box ([`crate::driver::vm_box`]), reached as the guest's own `ssh.run` steps reach
//! the VM — `ssh` to its address (`${outputs.ip}`) as the guest's user, with the runner's key, the
//! run's `known_hosts` and SSH configuration, through the run's jump and the step's `via`. The
//! outputs come from the run's Terraform state, so every operation (a status, a shell command
//! built without running anything) reaches the VM the same way.
//!
//! A run with no such guest (a VM lab on a cloud or a server: `cloud-vm`, Proxmox VMs) has no VM
//! to put the box in, and is refused.

use std::collections::BTreeMap;

use isoloom_manifest::{Action, Run};

use super::state;
use crate::Result;
use crate::driver::vm_box::{self, WayIn};
use crate::error::Error;
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::ssh::{self, Login};
use crate::status::ShellCommand;

/// The way into the guest VM: `ssh` at `login`.
pub(super) struct Guest {
    login: Login,
}

impl WayIn for Guest {
    fn spec(&self, cx: &Cx<'_>, _id: &str, command: &str) -> Result<ProcessSpec> {
        ssh::spec(cx, &self.login, command, Vec::new())
    }
}

/// What a run with no container host VM is told.
fn unsupported(what: &str) -> Error {
    Error::Unsupported {
        what: format!("{what} on a run with no container host VM (a VM lab on a server or a cloud)"),
    }
}

/// The way into the run's guest VM, `outputs` its Terraform outputs: the login of the guest's
/// `ssh.run` status step, filled. `None` when the run has no guest VM; refused when its guest is
/// reached some other way, and not running while its outputs give no address.
pub(super) fn guest(cx: &Cx<'_>, outputs: &BTreeMap<String, String>) -> Result<Option<Guest>> {
    let Some(g) = &cx.run.manifest().guest else { return Ok(None) };
    let Run::Action { action: Action::SshRun, with } = &g.status.run else {
        return Err(Error::Unsupported {
            what: "the attack box in a guest VM whose status isn't an `ssh.run` step: the runner has no way into it".to_owned(),
        });
    };
    let mut vars = cx.vars.clone();
    for (name, value) in outputs {
        vars.set_output(name, value.clone());
    }
    let arg = |name: &str| -> Result<String> { Ok(vars.with_arg(with, "guest.status", name)?.unwrap_or_default()) };
    let host = arg("host")?;
    if host.is_empty() {
        return Err(Error::NotRunning {
            run: cx.run.name().to_string(),
        });
    }
    Ok(Some(Guest {
        login: ssh::with_login(cx, &host, arg)?,
    }))
}

/// The way into the run's guest VM, its outputs read from the run's state; refused, naming
/// `what`, when the run has no such VM.
fn guest_of(cx: &Cx<'_>, what: &str) -> Result<Guest> {
    let tf = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?;
    guest(cx, &tf.outputs)?.ok_or_else(|| unsupported(what))
}

/// Stands the ad-hoc box up inside the guest VM ([`vm_box::attach`]).
pub(super) async fn attach(cx: &Cx<'_>, image: Option<&str>, network: Option<&str>) -> Result<crate::driver::Attached> {
    let way = guest_of(cx, "hot-adding an access machine")?;
    vm_box::attach(&way, cx, image, network).await
}

/// Removes the box from the guest VM ([`vm_box::detach`]).
pub(super) async fn detach(cx: &Cx<'_>) -> Result<()> {
    let way = guest_of(cx, "hot-removing an access machine")?;
    vm_box::detach(&way, cx).await
}

/// The command that opens an interactive shell into the box: `ssh -t` to the guest VM (the run's
/// SSH configuration, written now), then `sudo docker exec -it <name> …` there.
pub(super) fn shell_command(cx: &Cx<'_>) -> Result<ShellCommand> {
    let way = guest_of(cx, "a shell into an access machine")?;
    let spec = ssh::spec(cx, &way.login, &vm_box::exec_shell(cx), Vec::new())?;
    let mut args = vec!["-t".to_owned()];
    args.extend(spec.args);
    Ok(ShellCommand {
        program: spec.program,
        args,
        cwd: Some(cx.run.dir().to_path_buf()),
        env: Vec::new(),
    })
}
