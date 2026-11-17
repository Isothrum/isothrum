//! A module whose SSH (a provisioner, a `remote-exec`) reaches its machines only through a host
//! this computer reaches: a network internal to that host (a hypervisor's bridge with no route
//! from here). When the run names that host (its `ssh_jump` value, `user@host`), each `apply` and
//! `destroy` runs with a SOCKS proxy through it on this computer's loopback, `ssh -N -D`, named in
//! the module's `ssh_proxy_host` and `ssh_proxy_port` variables.
//!
//! - **Signing in.** With the run's `ssh_jump_key_file` value, else the runner's own key; the
//!   host's key pinned in the run's `known_hosts` the first time the run meets it, then checked
//!   strictly ([`crate::ssh::pin`]).
//! - **For how long.** The step's: the proxy's `ssh` is a tool of the operation, ended with its
//!   tree when the step ends, however it ends, and recorded in the run's tool ledger while it
//!   runs, so the next operation ends it after a crash of the process that started it.
//! - **Not opening.** A proxy that doesn't listen within [`OPENING`], or whose `ssh` ends before
//!   the step does, fails the step, in `ssh`'s own words.

use std::path::PathBuf;
use std::time::Duration;

use super::Module;
use super::route::JUMP_VAR;
use crate::Result;
use crate::error::Error;
use crate::events::Stream;
use crate::process::ProcessSpec;
use crate::ssh::Login;

/// The run's value naming the private key that signs in to the jump host.
pub(crate) const KEY_VAR: &str = "ssh_jump_key_file";

/// The module's variable naming the proxy's host.
pub(crate) const PROXY_HOST_VAR: &str = "TF_VAR_ssh_proxy_host";

/// The module's variable naming the proxy's port.
pub(crate) const PROXY_PORT_VAR: &str = "TF_VAR_ssh_proxy_port";

/// Where the proxy listens: this computer's loopback.
const LOOPBACK: &str = "127.0.0.1";

/// How long the proxy may take to listen: `ssh` listens once it has signed in to the host.
const OPENING: Duration = Duration::from_secs(30);

/// How often the proxy's port is tried while it opens.
const POLL: Duration = Duration::from_millis(200);

/// The step a proxy that doesn't open fails.
const STEP: &str = "jump";

/// The jump host the run names, and the key that signs in to it; none when it names none.
fn jump(module: &Module<'_, '_>) -> Result<Option<(Login, PathBuf)>> {
    let cx = module.cx;
    let Some(jump) = cx.var(JUMP_VAR) else { return Ok(None) };
    let refused = |why: String| Error::ToolFailed {
        step: STEP.to_owned(),
        reason: why,
    };
    let (user, host) = jump
        .split_once('@')
        .ok_or_else(|| refused(format!("the run's `{JUMP_VAR}` value `{}` isn't `user@host`", jump.escape_debug())))?;
    let login = Login::new(user, host).ok_or_else(|| refused(format!("the run's `{JUMP_VAR}` value `{}` isn't an SSH login", jump.escape_debug())))?;
    let key = cx.var(KEY_VAR).map_or_else(|| cx.ssh_key.to_path_buf(), PathBuf::from);
    Ok(Some((login, key)))
}

/// `ssh`'s process for the proxy through `login` on loopback port `port`, signing in with `key`.
fn proxy_spec(module: &Module<'_, '_>, login: &Login, key: &std::path::Path, port: u16) -> Result<ProcessSpec> {
    let mut args = crate::ssh::options(key, module.cx.state_dir)?;
    args.extend([
        "-N".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-D".to_owned(),
        format!("{LOOPBACK}:{port}"),
    ]);
    args.push(login.target());
    let mut spec = ProcessSpec::new("ssh", args);
    spec.cwd = Some(module.cx.state_dir.to_path_buf());
    Ok(spec)
}

/// A free port of this computer's loopback, now. Another program could take it before `ssh`
/// does: `ssh` then ends (`ExitOnForwardFailure`), and the step says so.
fn free_port() -> Result<u16> {
    let taken = std::net::TcpListener::bind((LOOPBACK, 0)).map_err(Error::io("a tcp port on 127.0.0.1", "bound"))?;
    Ok(taken.local_addr().map_err(Error::io("a tcp port on 127.0.0.1", "read"))?.port())
}

/// Runs `op` on `module`, through the proxy the run's jump host asks for, when it names one: `op`
/// gets the module with the proxy's variables, and the proxy ends with it.
///
/// # Errors
///
/// As `op`; `tool_failed` (step `jump`) when the run's jump host isn't a login or the proxy doesn't
/// open or ends first; as pinning the host's key fails.
pub(super) async fn through<T>(module: &Module<'_, '_>, op: impl AsyncFnOnce(&Module<'_, '_>) -> Result<T>) -> Result<T> {
    let Some((login, key)) = jump(module)? else { return op(module).await };
    let cx = module.cx;
    crate::ssh::pin(cx.processes, cx.state_dir, &login.host).await?;
    let port = free_port()?;
    let spec = proxy_spec(module, &login, &key, port)?;
    let said = std::sync::Mutex::new(String::new());
    let mut keep = |stream: Stream, line: &str| {
        if stream == Stream::Stderr && !line.trim().is_empty() {
            line.trim().clone_into(&mut said.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        }
    };
    let ssh = cx.processes.run(&spec, &mut keep);
    tokio::pin!(ssh);
    let ended = |how: String| {
        let last = said.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let because = if last.is_empty() { String::new() } else { format!(": ssh said `{last}`") };
        Error::ToolFailed {
            step: STEP.to_owned(),
            reason: format!("the SSH proxy through {} {how}{because}", login.target()),
        }
    };
    let deadline = tokio::time::Instant::now() + OPENING;
    loop {
        if tokio::net::TcpStream::connect((LOOPBACK, port)).await.is_ok() {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(ended(format!("didn't open within {}s", OPENING.as_secs())));
        }
        tokio::select! {
            exit = &mut ssh => return Err(ended(exit.map_or_else(|e| e.to_string(), |exit| format!("ended ({exit})")))),
            () = tokio::time::sleep(POLL) => {}
        }
    }
    let proxied = module.with_env(&[(PROXY_HOST_VAR, LOOPBACK.to_owned()), (PROXY_PORT_VAR, port.to_string())]);
    tokio::select! {
        done = op(&proxied) => done,
        exit = &mut ssh => Err(ended(exit.map_or_else(|e| e.to_string(), |exit| format!("ended during the step ({exit})")))),
    }
}
