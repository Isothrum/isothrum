//! An SSH tunnel to a machine's console (`source.from: ssh_tunnel`): `ssh -N -L
//! 127.0.0.1:<local>:<host>:<port>` to the machine it goes through, with the run's key and pinned
//! host keys (its `ssh_config`), the run's jump first. It listens on this computer's loopback
//! only, at a free port, and lives as long as the [`Tunnel`] (its `ssh` goes with it). Not the
//! [`crate::ProcessRunner`] port: like the macOS relays' `nc`, it is a pipe for as long as a
//! viewer is connected, not a tool run to its end.

use std::net::SocketAddr;
use std::time::Duration;

use isoloom_manifest::{Machine, Protocol};

use super::{LOOPBACK, none};
use crate::Result;
use crate::executor::Cx;
use crate::process::ProcessSpec;

/// How long the tunnel may take to listen.
const OPENING: Duration = Duration::from_secs(20);

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
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // `kill_on_drop` too: the tunnel ends with its holder, whatever drops it.
        if let Err(e) = self.ssh.start_kill() {
            tracing::debug!(error = %e, "a console's tunnel had ended already");
        }
    }
}

/// `ssh`'s process for a tunnel from `local` (a loopback port) to `to`.
pub(super) fn spec(cx: &Cx<'_>, to: &To, local: u16) -> Result<ProcessSpec> {
    let login = crate::ssh::Login::new(&to.user, &to.via, cx.ssh_jump.as_deref(), "")?;
    if !to.host.chars().all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c)) || to.host.is_empty() {
        return Err(crate::error::Error::Malformed {
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
/// port answering within [`OPENING`].
pub(super) async fn open(cx: &Cx<'_>, machine: &Machine, to: &To) -> Result<Tunnel> {
    let ports = &cx.run.runner.config.ports;
    let Ok(Some(free)) = ports.pick_free(Protocol::Tcp) else {
        return Err(none(machine, "no free port on this computer's loopback for its tunnel"));
    };
    let local = free.port;
    drop(free);
    let spec = spec(cx, to, local)?;
    let tunnel = start(&spec, SocketAddr::new(LOOPBACK, local)).map_err(|e| none(machine, format!("its tunnel's `ssh` didn't start: {e}")))?;
    if !ports.reachable(&LOOPBACK.to_string(), local, OPENING).await {
        return Err(none(
            machine,
            format!(
                "its tunnel through {} didn't open (SSH refused or {}:{} doesn't answer)",
                to.via, to.host, to.port
            ),
        ));
    }
    Ok(tunnel)
}

/// Starts `spec` as a tunnel listening at `local`, stopped when the tunnel is dropped.
pub(super) fn start(spec: &ProcessSpec, local: SocketAddr) -> std::io::Result<Tunnel> {
    let mut command = tokio::process::Command::new(&spec.program);
    command
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    Ok(Tunnel { local, ssh: command.spawn()? })
}

#[cfg(test)]
mod tests {
    use super::*;

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
