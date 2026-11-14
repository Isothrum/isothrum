//! A way into a machine of a Terraform run, through the run's SSH configuration and jump: a VM's
//! console is `ssh -t` to its public address as its user (the module's `address_<machine>` and
//! `ssh_user_<machine>` outputs, as the guest steps reach it), then a login shell (a `sudo -i` one
//! as the account the embedder names, when it names one); a script runs
//! in the run's VM (cloud-docker, Proxmox docker-vm) over `ssh` to that VM, as the guest's own
//! steps reach it.

use isoloom_manifest::Machine;

use super::state;
use crate::Result;
use crate::console::ShellCommand;
use crate::driver::{console_in_run, shells};
use crate::error::Error;
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::ssh::{self, Login};

/// The command that opens a console into `machine`, a VM the module makes, as `user` when given
/// ([`shells::login_shell_as`]); not running while the state gives it no address.
pub(super) fn command(cx: &Cx<'_>, machine: &Machine, user: Option<&str>) -> Result<ShellCommand> {
    if crate::driver::is_compose_service(machine) {
        return Err(crate::driver::no_vm_for("a console into", machine));
    }
    let outputs = state::read(cx.state_dir())?.outputs;
    let login = ssh::machine_login(|name| outputs.get(name).cloned(), machine.name.as_str(), cx.ssh_jump())?.ok_or_else(|| Error::NotRunning {
        run: cx.run.name().to_string(),
    })?;
    let shell = user.map_or_else(|| shells::LOGIN_SHELL.to_owned(), shells::login_shell_as);
    Ok(console_in_run(cx, over_ssh(cx, &login, &shell)?))
}

/// `ssh` at `login`, running `command` there with a terminal (`-t`).
fn over_ssh(cx: &Cx<'_>, login: &Login, command: &str) -> Result<ProcessSpec> {
    let mut spec = ssh::spec(cx, login, command, Vec::new())?;
    spec.args.insert(0, "-t".to_owned());
    Ok(spec)
}
