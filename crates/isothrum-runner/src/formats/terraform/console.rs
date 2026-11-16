//! A console into a module's VM, and a command run in it: an `ssh` along the route the module's
//! outputs and the run's values give ([`super::route`]), the first host on the way pinned first.

use super::route::{self, Route};
use super::{Module, Ran};
use crate::Result;
use crate::cli::CliError;
use crate::console::ShellCommand;
use crate::error::Error;
use crate::graphical::no_console;

/// The exit status `ssh` ends with when it couldn't run the command (no connection, a key
/// refused): the command's own is any other.
const SSH_FAILED: i32 = 255;

/// The console into `machine`, signed in as `user` when given.
///
/// # Errors
///
/// `machine_unknown` when the module has no such VM; `no_console` when there is no route to it
/// ([`route`]); as the state's and the outputs' reads and the host key's pin fail.
pub(super) async fn of(module: &Module<'_, '_>, machine: &str, user: Option<&str>) -> Result<ShellCommand> {
    let route = route::to(module, machine, user).await?.map_err(|why| no_console(machine, why))?;
    let mut args = route.options(module).await?;
    args.extend(["-t".to_owned(), route.to.target()]);
    Ok(ShellCommand::new("ssh", args, module.cx.project.dir.clone()))
}

/// Runs `argv` in `machine` as the module's user there, with no terminal: what it printed and how
/// it ended. A command that ran and failed is its exit status, not an error.
///
/// # Errors
///
/// `machine_unknown` when the module has no such VM; `unsupported` when there is no route to it
/// ([`route`]); `tool_failed` when `ssh` couldn't run it (it ended with 255); as the reads, the
/// host key's pin and `ssh` itself fail.
pub(super) async fn exec(module: &Module<'_, '_>, machine: &str, argv: &[String]) -> Result<Ran> {
    let route: Route = route::to(module, machine, None).await?.map_err(|why| Error::Unsupported {
        what: format!("running a command in `{machine}`: {why}"),
    })?;
    let mut ssh_args = route.options(module).await?;
    let command = argv.iter().map(|w| crate::console::sh_word(w)).collect::<Vec<_>>().join(" ");
    ssh_args.extend([route.to.target(), command]);
    let call = module.cx.call(super::super::CHECK_LIMIT);
    let out = call.run::<String>("ssh", ssh_args).await.map_err(|e| module.error("exec", e))?;
    let exit = out.exit.code.unwrap_or(-1);
    if exit == SSH_FAILED {
        return Err(module.error(
            "exec",
            CliError::Failed {
                tool: "ssh",
                failure: format!("it couldn't run the command in `{machine}` ({}): {}", route.to.target(), out.reason()),
            },
        ));
    }
    Ok(Ran {
        exit,
        stdout: out.stdout.join("\n"),
        stderr: out.stderr_tail.join("\n"),
    })
}
