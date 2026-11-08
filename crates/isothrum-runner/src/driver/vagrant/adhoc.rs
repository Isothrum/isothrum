//! The ad-hoc attacker box of a container lab in a Vagrant VM (`docker-vm`, the manifest's
//! `guest`): the shared in-VM box ([`crate::driver::vm_box`]), reached through
//! `vagrant ssh <vm> --command "…"` with the run's state folder (`VAGRANT_DOTFILE_PATH`).

use isoloom_manifest::Native;

use super::{Vagrant, define_name};
use crate::Result;
use crate::driver::vm_box::{self, WayIn};
use crate::driver::{Attached, Driver};
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::status::{MachineStatus, ShellCommand};

pub(super) use vm_box::SHELL;

/// The way into the VM `vm` (its define name): `vagrant ssh --no-tty --command <command> <vm>`.
struct InVm<'a> {
    v: &'a Vagrant,
    vm: &'a str,
}

impl WayIn for InVm<'_> {
    fn spec(&self, cx: &Cx<'_>, id: &str, command: &str) -> Result<ProcessSpec> {
        self.v.vagrant(cx, id, &["ssh", "--no-tty", "--command", command, self.vm])
    }
}

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

/// Stands the ad-hoc box up inside `vm` ([`vm_box::attach`]).
pub(super) async fn attach(v: &Vagrant, cx: &Cx<'_>, vm: &str, image: Option<&str>, network: Option<&str>) -> Result<Attached> {
    vm_box::attach(&InVm { v, vm }, cx, image, network).await
}

/// Removes the box from `vm` ([`vm_box::detach`]).
pub(super) async fn detach(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Result<()> {
    vm_box::detach(&InVm { v, vm }, cx).await
}

/// The ad-hoc box in `vm` as a status machine ([`vm_box::status`]).
pub(super) async fn status(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Option<MachineStatus> {
    vm_box::status(&InVm { v, vm }, cx).await
}

/// The command that opens an interactive shell into the box: `vagrant ssh <vm> -c "sudo docker
/// exec -it <name> …"`, in the run's folder, with the run's Vagrant environment (its state folder,
/// its forwarded ports).
pub(super) fn shell_command(v: &Vagrant, cx: &Cx<'_>, vm: &str) -> Result<ShellCommand> {
    Ok(ShellCommand {
        program: "vagrant".into(),
        args: vec!["ssh".into(), vm.to_owned(), "-c".into(), vm_box::exec_shell(cx)],
        cwd: Some(cx.run.dir().to_path_buf()),
        env: v.env(cx)?,
    })
}
