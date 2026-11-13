//! An SSH tunnel to a machine's console (`source.from: ssh_tunnel`): `ssh -N -L
//! 127.0.0.1:<local>:<host>:<port>` to the machine it goes through, with the run's key and pinned
//! host keys (its `ssh_config`), the run's jump first. It listens on this computer's loopback
//! only, at a free port, and lives as long as the [`Tunnel`] (its `ssh` goes with it). Not the
//! [`crate::ProcessRunner`] port: like the macOS relays' `nc`, it is a pipe for as long as a
//! viewer is connected, not a tool run to its end.

use std::net::SocketAddr;
use std::time::Duration;

use isoloom_manifest::{Machine, Protocol};

use super::{LOOPBACK, no_console};
use crate::Result;
use crate::error::Error;
use crate::executor::Cx;
use crate::process::ProcessSpec;

/// How long the tunnel may take to listen.
const OPENING: Duration = Duration::from_secs(20);

/// The step id a missing `ssh` is reported under.
const STEP: &str = "console";

/// Where a tunnel goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct To {
    /// The SSH host it goes through.
    pub(super) via: String,
    /// The user there.
    pub(super) user: String,
    /// The machine's address, as `via` reaches it.
    pub(super) host: String,
    /// The machine's port.
    pub(super) port: u16,
}

/// An open tunnel: its loopback port, and its `ssh`, stopped when it is dropped.
#[derive(Debug)]
pub(crate) struct Tunnel {
    /// Where it listens.
    pub(super) local: SocketAddr,
    /// Its `ssh`, killed when the tunnel is dropped.
    ssh: tokio::process::Child,
    /// The reader of its `ssh`'s standard error, owned so it ends with the tunnel (Clean Code
    /// ch.13, shutdown; 2026-11-13: it was spawned detached).
    drain: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // `kill_on_drop` too: the tunnel ends with its holder, whatever drops it.
        if let Err(e) = self.ssh.start_kill() {
            tracing::debug!(error = %e, "a console's tunnel had ended already");
        }
        if let Some(drain) = self.drain.take() {
            drain.abort();
        }
    }
}

/// `ssh`'s process for a tunnel from `local` (a loopback port) to `to`.
pub(super) fn spec(cx: &Cx<'_>, to: &To, local: u16) -> Result<ProcessSpec> {
    let login = crate::ssh::Login::new(&to.user, &to.via, cx.ssh_jump.as_deref(), "")?;
    if !to.host.chars().all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c)) || to.host.is_empty() {
        return Err(Error::Malformed {
            at: "a console's tunnel".to_owned(),
            reason: format!("`{}` isn't an address", to.host),
        });
    }
    let forward = format!("{LOOPBACK}:{local}:{}:{}", bracketed(&to.host), to.port);
    let mut spec = crate::ssh::spec(cx, &login, "", Vec::new())?;
    // `ssh -F <config> [-J …] <login> ""`: no command, a forward instead.
    spec.args.pop();
    let target = spec.args.pop().unwrap_or_default();
    spec.args.extend(["-N", "-o", "ExitOnForwardFailure=yes", "-L", &forward].map(String::from));
    spec.args.push(target);
    Ok(spec)
}

/// An IPv6 address in brackets, as `-L` takes it.
fn bracketed(host: &str) -> String {
    if host.contains(':') { format!("[{host}]") } else { host.to_owned() }
}

/// Opens a tunnel to `to` for `machine`'s console: a free loopback port, `ssh` started, and the
/// port answering within [`OPENING`]; when it doesn't, why, in `ssh`'s own words when it said any.
pub(super) async fn open(cx: &Cx<'_>, machine: &Machine, to: &To) -> Result<Tunnel> {
    let free = cx
        .run
        .runner
        .pick_unreserved(cx.run.name(), Protocol::Tcp)
        .map_err(Error::io("a tcp port on 127.0.0.1", "bound"))?
        .ok_or_else(|| no_console(machine, "no free port on this computer's loopback for its tunnel"))?;
    let local = free.port;
    drop(free);
    let mut tunnel = started(machine, &spec(cx, to, local)?, SocketAddr::new(LOOPBACK, local))?;
    if !cx.ports().reachable(&LOOPBACK.to_string(), local, OPENING).await {
        let said = tunnel.why_closed().await;
        let because = said.map_or_else(String::new, |line| format!(": ssh said `{}`", cx.events.mask(&line)));
        return Err(no_console(
            machine,
            format!(
                "its tunnel through {} didn't open (SSH refused or {}:{} doesn't answer){because}",
                to.via, to.host, to.port
            ),
        ));
    }
    tunnel.drain();
    Ok(tunnel)
}

/// [`start`] for `machine`'s console: a program that isn't installed is `tool_missing`, another
/// failure to start it `no_console` saying why.
fn started(machine: &Machine, spec: &ProcessSpec, local: SocketAddr) -> Result<Tunnel> {
    start(spec, local).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::ToolMissing {
            step: STEP.to_owned(),
            tool: spec.program.clone(),
        },
        _ => no_console(machine, format!("its tunnel's `{}` didn't start: {e}", spec.program)),
    })
}

/// Starts `spec` as a tunnel listening at `local`, stopped when the tunnel is dropped; its
/// standard error kept for [`Tunnel::why_closed`].
pub(super) fn start(spec: &ProcessSpec, local: SocketAddr) -> std::io::Result<Tunnel> {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    Ok(Tunnel {
        local,
        ssh: command.spawn()?,
        drain: None,
    })
}

impl Tunnel {
    /// The last line `ssh` wrote to its standard error, once it is stopped: why a tunnel that
    /// didn't open closed (a host key that changed, a key refused). None when it said nothing.
    async fn why_closed(&mut self) -> Option<String> {
        use tokio::io::AsyncReadExt;
        // An `ssh` that failed has exited, or is about to: it is given a moment to say why.
        if tokio::time::timeout(Duration::from_secs(2), self.ssh.wait()).await.is_err()
            && let Err(e) = self.ssh.start_kill()
        {
            tracing::debug!(error = %e, "a console's tunnel couldn't be stopped");
        }
        let mut stderr = self.ssh.stderr.take()?;
        let mut said = Vec::new();
        // Its end comes with the process's; a grandchild holding the pipe open is waited for no
        // longer than this.
        if let Ok(Err(e)) = tokio::time::timeout(Duration::from_secs(2), stderr.read_to_end(&mut said)).await {
            tracing::debug!(error = %e, "a console's tunnel's standard error couldn't be read");
        }
        String::from_utf8_lossy(&said)
            .lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(str::to_owned)
    }

    /// Reads what `ssh` writes to its standard error while the tunnel is open, into the trace, so
    /// a full pipe never stalls it.
    fn drain(&mut self) {
        let Some(stderr) = self.ssh.stderr.take() else { return };
        self.drain = Some(tokio::spawn(async move {
            use tokio::io::AsyncBufReadExt;
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(%line, "a console's tunnel's ssh said");
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_ssh_is_a_tool_missing() {
        // Regression: any failure to start it, a missing program included, was `no_console`.
        let web: Machine = serde_json::from_value(serde_json::json!({ "name": "web", "role": "member", "addresses": {} })).unwrap();
        let spec = ProcessSpec::new("isothrum-no-such-ssh", ["-N"]);
        let err = started(&web, &spec, "127.0.0.1:1".parse().unwrap()).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::ToolMissing, "{err}");
        assert!(err.to_string().contains("isothrum-no-such-ssh"), "{err}");
    }

    /// Regression: `ssh`'s standard error went to null, so a tunnel that didn't open never said
    /// why (a host key that changed, a key refused).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tunnel_that_closed_says_what_ssh_said() {
        let spec = ProcessSpec::new("sh", ["-c", "echo 'debug1: x' >&2; echo 'Host key verification failed.' >&2; exit 255"]);
        let mut tunnel = start(&spec, "127.0.0.1:1".parse().unwrap()).unwrap();
        assert_eq!(tunnel.why_closed().await.as_deref(), Some("Host key verification failed."));
        let quiet = ProcessSpec::new("sh", ["-c", "exit 255"]);
        assert_eq!(start(&quiet, "127.0.0.1:1".parse().unwrap()).unwrap().why_closed().await, None);
    }

    #[test]
    fn an_ipv6_host_is_bracketed_in_the_forward() {
        assert_eq!(bracketed("fd00::5"), "[fd00::5]");
        assert_eq!(bracketed("10.0.1.5"), "10.0.1.5");
    }

    /// A tunnel's process lives as long as the tunnel: dropping it stops the process.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tunnel_stops_its_process_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let mark = dir.path().join("alive");
        let script = format!("touch {}; sleep 30", mark.display());
        let spec = ProcessSpec::new("sh", ["-c", script.as_str()]);
        let tunnel = start(&spec, "127.0.0.1:1".parse().unwrap()).unwrap();
        let pid = tunnel.ssh.id().unwrap();
        for _ in 0..100 {
            if mark.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(mark.exists(), "the process ran");
        drop(tunnel);
        tokio::time::sleep(Duration::from_millis(200)).await;
        // Gone, or a zombie its parent hasn't reaped yet: either way it no longer runs.
        let ps = std::process::Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
        let stat = String::from_utf8_lossy(&ps.stdout).trim().to_owned();
        assert!(stat.is_empty() || stat.starts_with('Z'), "the process went with the tunnel: {stat}");
    }
}
