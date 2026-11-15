//! A console into a module's VM: an `ssh` to the address its state says, as the user the
//! module's outputs name, with the run's key, the machine's host key pinned first. The rules are
//! the tunnels' ([`super::tunnelled`]):
//!
//! - **Where.** The VM's address, as its provider writes it in the state.
//! - **As whom.** The user asked for, else `ssh_user_<machine>` of the machine whose
//!   `address_<machine>` that address is, else `ssh_user`.
//! - **With which key.** The run's `ssh_private_key_file` value, else the runner's own key.
//!
//! A Windows VM has none: its screen is its Remote Desktop.

use std::path::PathBuf;

use super::{Module, tunnelled, vm_named};
use crate::Result;
use crate::console::ShellCommand;
use crate::graphical::no_console;
use crate::ssh::Login;

/// The console into `machine`, signed in as `user` when given.
///
/// # Errors
///
/// `machine_unknown` when the module has no such VM; `no_console` when its state gives it no
/// address, the outputs name no user, or it runs Windows; as the state's and the outputs' reads
/// and the host key's pin fail.
pub(super) async fn of(module: &Module<'_, '_>, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
    let cx = module.cx;
    let state = module.state()?;
    let held = module.shown(state).await?;
    let vm = vm_named(cx, &held, machine)?;
    if vm.vm.windows {
        return Err(no_console(machine, "it runs Windows: its screen is its Remote Desktop"));
    }
    let address = vm
        .vm
        .address
        .clone()
        .ok_or_else(|| no_console(machine, "its state gives it no address to reach"))?;
    let out = tunnelled::outputs(module, state).await?;
    let user = user
        .map(str::to_owned)
        .or_else(|| out.get(&format!("ssh_user_{machine}")).cloned())
        .or_else(|| tunnelled::user_at(&out, &address))
        .ok_or_else(|| no_console(machine, format!("the module names no SSH user for it (`ssh_user_{machine}`, `ssh_user`)")))?;
    let login =
        Login::new(&user, &address).ok_or_else(|| no_console(machine, format!("`{}@{}` isn't an SSH login", user.escape_debug(), address.escape_debug())))?;
    let key = cx.var(tunnelled::KEY_VAR).map_or_else(|| cx.ssh_key.to_path_buf(), PathBuf::from);
    crate::ssh::pin(cx.processes, cx.state_dir, &address).await?;
    let mut args = crate::ssh::options(&key, cx.state_dir)?;
    args.extend(["-t".to_owned(), login.target()]);
    Ok(ShellCommand::new("ssh", args, cx.project.dir.clone()))
}
