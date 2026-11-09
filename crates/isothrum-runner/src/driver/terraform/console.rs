//! A way into a machine of a Terraform run, through the run's SSH configuration and jump: a VM's
//! console is `ssh -t` to its public address as its user (the module's `address_<machine>` and
//! `ssh_user_<machine>` outputs, as the guest steps reach it), then a login shell; a script runs
//! in the run's VM (cloud-docker, Proxmox docker-vm) over `ssh` to that VM, as the guest's own
//! steps reach it.

use isoloom_manifest::Machine;

use super::state;
use crate::Result;
use crate::driver::{GuestUse, console_in_run, shells};
use crate::error::Error;
use crate::executor::Cx;
use crate::process::ProcessSpec;
use crate::ssh::{self, Login};
use crate::status::ShellCommand;

/// The command that opens a console into `machine`, a VM the module makes; not running while
/// the state gives it no address.
pub(super) fn command(cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand> {
    if machine.native.is_some() {
        return Err(Error::Unsupported {
            what: format!("a console into `{}`: a Compose service with no VM to run in", machine.name),
        });
    }
    let outputs = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs;
    let login = ssh::machine_login(|name| outputs.get(name).cloned(), machine.name.as_str(), cx.ssh_jump.as_deref())?.ok_or_else(|| Error::NotRunning {
        run: cx.run.name().to_string(),
    })?;
    Ok(console_in_run(cx, over_ssh(cx, &login, shells::LOGIN_SHELL, true)?))
}

/// `script` run in the run's VM over `ssh`, with a terminal for a console; refused when the run
/// has none. Either way the session carries no variable: a Terraform run delivers nothing to its
/// VM's sessions.
pub(super) fn in_guest(cx: &Cx<'_>, script: &str, use_: GuestUse) -> Result<ProcessSpec> {
    let outputs = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs;
    let login = super::guest::login(cx, &outputs)?.ok_or_else(|| Error::Unsupported {
        what: "running a script in the run's VM: this run has none".to_owned(),
    })?;
    over_ssh(cx, &login, script, use_ == GuestUse::Console)
}

/// `ssh` at `login` (`-t` with `tty`), running `command` there.
fn over_ssh(cx: &Cx<'_>, login: &Login, command: &str, tty: bool) -> Result<ProcessSpec> {
    let mut spec = ssh::spec(cx, login, command, Vec::new())?;
    if tty {
        spec.args.insert(0, "-t".to_owned());
    }
    Ok(spec)
}
