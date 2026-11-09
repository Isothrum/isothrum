//! A console into a machine of a Terraform run, through the run's SSH configuration and jump: a
//! VM is `ssh -t` to its public address as its user (the module's `address_<machine>` and
//! `ssh_user_<machine>` outputs, as the guest steps reach it), then a login shell; a Compose
//! service running in the run's VM (cloud-docker, Proxmox docker-vm) is `ssh -t` to that VM,
//! then into the service's container.

use isoloom_manifest::Machine;

use super::state;
use crate::Result;
use crate::driver::console;
use crate::error::Error;
use crate::executor::Cx;
use crate::ssh::{self, Login};
use crate::status::ShellCommand;

/// The command that opens a console into `machine`; not running while the state gives it no
/// address.
pub(super) fn command(cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand> {
    let outputs = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs;
    if let Some(isoloom_manifest::Native::Compose { service }) = &machine.native {
        let login = super::guest::login(cx, &outputs)?.ok_or_else(|| Error::Unsupported {
            what: format!("a console into `{}`: a Compose service with no VM to run in", machine.name),
        })?;
        return over_ssh(cx, &login, &console::into_service(service.as_str()));
    }
    let key = machine.name.as_str().replace('-', "_");
    let host = outputs
        .get(&format!("address_{key}"))
        .filter(|a| !a.is_empty())
        .ok_or_else(|| Error::NotRunning {
            run: cx.run.name().to_string(),
        })?;
    let user = ssh::machine_user(|name| outputs.get(name).cloned(), &key);
    let login = Login::new(&user, host, cx.ssh_jump.as_deref(), "")?;
    over_ssh(cx, &login, console::LOGIN_SHELL)
}

/// Removes `machine`, a Compose service in the run's VM, there with the services in its
/// namespace, over SSH; nothing for a machine the module makes (the update's apply removes it).
pub(super) async fn retire(cx: &Cx<'_>, machine: &Machine) -> Result<()> {
    let Some(isoloom_manifest::Native::Compose { service }) = &machine.native else {
        return Ok(());
    };
    let outputs = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs;
    let Some(login) = super::guest::login(cx, &outputs)? else { return Ok(()) };
    let spec = ssh::spec(cx, &login, &console::remove_service(service.as_str()), Vec::new())?;
    let op = crate::state::Op::Retire.as_str();
    crate::executor::call_ok(cx, op, &spec, RETIRE_TIMEOUT, crate::executor::Echo::Shown)
        .await
        .map(drop)
}

/// How long removing a service in the run's VM may take.
const RETIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// `ssh -t` at `login`, running `command` there.
fn over_ssh(cx: &Cx<'_>, login: &Login, command: &str) -> Result<ShellCommand> {
    let spec = ssh::spec(cx, login, command, Vec::new())?;
    let mut args = vec!["-t".to_owned()];
    args.extend(spec.args);
    Ok(ShellCommand {
        program: spec.program,
        args,
        cwd: Some(cx.run.dir().to_path_buf()),
        env: Vec::new(),
    })
}
