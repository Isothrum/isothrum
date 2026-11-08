//! A console into a machine of a Terraform run: `ssh -t` to the machine's public address as its
//! user (the module's `address_<machine>` and `ssh_user_<machine>` outputs, as the guest steps
//! reach it), through the run's SSH configuration and jump, then a login shell there.

use isoloom_manifest::Machine;

use super::state;
use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::ssh::{self, Login};
use crate::status::ShellCommand;

/// What runs on the machine: a login shell, bash where it has one.
const LOGIN_SHELL: &str = "exec bash -l 2>/dev/null || exec sh -l";

/// The command that opens a console into `machine`; not running while the state gives it no
/// address.
pub(super) fn command(cx: &Cx<'_>, machine: &Machine) -> Result<ShellCommand> {
    let outputs = state::read(&cx.run.runner.store.run_dir(cx.run.name())?)?.outputs;
    let key = machine.name.as_str().replace('-', "_");
    let host = outputs
        .get(&format!("address_{key}"))
        .filter(|a| !a.is_empty())
        .ok_or_else(|| Error::NotRunning {
            run: cx.run.name().to_string(),
        })?;
    let user = ssh::machine_user(|name| outputs.get(name).cloned(), &key);
    let login = Login::new(&user, host, cx.ssh_jump.as_deref(), "")?;
    let spec = ssh::spec(cx, &login, LOGIN_SHELL, Vec::new())?;
    let mut args = vec!["-t".to_owned()];
    args.extend(spec.args);
    Ok(ShellCommand {
        program: spec.program,
        args,
        cwd: Some(cx.run.dir().to_path_buf()),
        env: Vec::new(),
    })
}
