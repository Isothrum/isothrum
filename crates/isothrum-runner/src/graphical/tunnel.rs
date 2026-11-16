//! An SSH tunnel to a machine's console that answers only on the run's own networks (a Windows
//! VM's Remote Desktop behind the module's Linux machine): `ssh -N -L
//! 127.0.0.1:<local>:<host>:<port>` to the machine it goes through, with the run's key and the
//! run's pinned host keys ([`crate::ssh`]). It listens on this computer's loopback only, at a
//! free port no run holds, and lives as long as its [`Tunnel`]: dropping it ends its `ssh`. Its
//! process is recorded in the run's tool ledger while it lives, so the next operation after a
//! crash of the process holding it ends it, and so does a `down` from any process.
//!
//! Not the [`crate::ProcessRunner`] port: a tunnel is a pipe for as long as a viewer is
//! connected, not a tool run to its end.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::no_console;
use crate::Result;
use crate::error::Error;
use crate::name::RunName;
use crate::process::ProcessSpec;
use crate::process::ledger::Recorded;
use crate::project::platform::Protocol;
use crate::runner::Runner;
use crate::ssh::Login;

/// Where a tunnel listens: this computer's loopback.
const LOOPBACK: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// How long the tunnel may take to listen.
const OPENING: Duration = Duration::from_secs(20);

/// How long a tunnel that didn't open is given to exit, then to finish writing why.
const SAYING_WHY: Duration = Duration::from_secs(2);

/// The step id a missing `ssh` is reported under.
const STEP: &str = "console";

/// Where a tunnel goes, as a driver reads it from its tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct To {
    /// The machine it goes through, and the user there.
    pub(crate) via: Login,
    /// The console's machine's address, as `via` reaches it.
    pub(crate) host: String,
    /// The console's port there.
    pub(crate) port: u16,
    /// The private key that signs in to `via` (the run's `ssh_private_key_file`); none: the
    /// runner's own.
    pub(crate) key: Option<PathBuf>,
}

/// An open tunnel to a machine's console: where it listens, and its `ssh`, ended when it is
/// dropped (and its ledger entry removed).
#[derive(Debug)]
pub struct Tunnel {
    /// Where it listens.
    local: SocketAddr,
    /// `user@host` it goes through.
    via: String,
    /// Its `ssh`, killed when the tunnel is dropped.
    ssh: tokio::process::Child,
    /// The reader of its `ssh`'s standard error, owned so it ends with the tunnel.
    drain: Option<tokio::task::JoinHandle<()>>,
    /// Its entry in the run's tool ledger, removed after its `ssh` is ended.
    _recorded: Option<Recorded>,
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

impl Tunnel {
    /// Where it listens: a port of this computer's loopback.
    #[must_use]
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// The machine it goes through, as `user@host`.
    #[must_use]
    pub fn via(&self) -> &str {
        &self.via
    }

    /// Waits for its `ssh` to end (the connection dropped, the machine it goes through gone, a
    /// `down`), and says how it ended: the tunnel no longer listens. A viewer waits on it beside
    /// its own end.
    pub async fn closed(&mut self) -> String {
        match self.ssh.wait().await {
            Ok(status) => match status.code() {
                Some(code) => format!("its ssh exited with code {code}"),
                None => "its ssh was ended by a signal".to_owned(),
            },
            Err(e) => format!("its ssh couldn't be waited for: {e}"),
        }
    }

    /// The last line `ssh` wrote to its standard error, once it is stopped: why a tunnel that
    /// didn't open closed (a host key that changed, a key refused). None when it said nothing.
    async fn why_closed(&mut self) -> Option<String> {
        use tokio::io::AsyncReadExt;
        // An `ssh` that failed has exited, or is about to: it is given a moment to say why.
        if tokio::time::timeout(SAYING_WHY, self.ssh.wait()).await.is_err()
            && let Err(e) = self.ssh.start_kill()
        {
            tracing::debug!(error = %e, "a console's tunnel couldn't be stopped");
        }
        let mut stderr = self.ssh.stderr.take()?;
        let mut said = Vec::new();
        if let Ok(Err(e)) = tokio::time::timeout(SAYING_WHY, stderr.read_to_end(&mut said)).await {
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

/// `ssh`'s process for a tunnel to `machine`'s console from loopback port `local` to `to`, signing
/// in with `key`, the run's state folder `state_dir` holding its pinned host keys.
fn ssh_spec(machine: &str, to: &To, key: &Path, state_dir: &Path, local: u16) -> Result<ProcessSpec> {
    let address = |h: &str| !h.is_empty() && !h.starts_with('-') && h.chars().all(|c| c.is_ascii_alphanumeric() || ".:-".contains(c));
    if !address(&to.host) {
        return Err(no_console(
            machine,
            format!("its address `{}` isn't one a tunnel goes to", to.host.escape_debug()),
        ));
    }
    let forward = format!("{LOOPBACK}:{local}:{}:{}", bracketed(&to.host), to.port);
    let mut args = crate::ssh::options(key, state_dir)?;
    args.extend(["-N", "-o", "ExitOnForwardFailure=yes", "-L", &forward].map(String::from));
    args.push(to.via.target());
    let mut spec = ProcessSpec::new("ssh", args);
    spec.cwd = Some(state_dir.to_path_buf());
    Ok(spec)
}

/// An IPv6 address in brackets, as `-L` takes it.
fn bracketed(host: &str) -> String {
    if host.contains(':') { format!("[{host}]") } else { host.to_owned() }
}

/// Opens a tunnel to `to` for run `run`'s `machine`'s console: `via`'s host key pinned the first
/// time ([`crate::ssh::pin`]), a free loopback port no run holds, `ssh` started and recorded in
/// the run's tool ledger, and the port answering within [`OPENING`]; when it doesn't, why, in
/// `ssh`'s own words when it said any.
pub(crate) async fn open(runner: &Runner, run: &RunName, state_dir: &Path, machine: &str, to: &To) -> Result<Tunnel> {
    let config = &runner.config;
    crate::ssh::pin(config.processes.as_ref(), state_dir, &to.via.host).await?;
    let key = to.key.clone().unwrap_or_else(|| crate::ssh::key_path(&config.home));
    let free = runner
        .pick_unreserved(run, Protocol::Tcp)
        .map_err(Error::io("a tcp port on 127.0.0.1", "bound"))?
        .ok_or_else(|| no_console(machine, "no free port on this computer's loopback for its tunnel"))?;
    let local = free.port.get();
    // Freed for `ssh` to bind it.
    drop(free);
    let spec = ssh_spec(machine, to, &key, state_dir, local)?;
    let ledger = state_dir.join(crate::process::ledger::DIR);
    let mut tunnel = started(machine, &spec, SocketAddr::new(LOOPBACK, local), &to.via.target(), &ledger)?;
    if !config.ports.reachable(&LOOPBACK.to_string(), local, OPENING).await {
        let because = tunnel.why_closed().await.map_or_else(String::new, |line| format!(": ssh said `{line}`"));
        return Err(no_console(
            machine,
            format!(
                "its tunnel through {} didn't open (SSH refused, or {}:{} doesn't answer from there){because}",
                tunnel.via, to.host, to.port
            ),
        ));
    }
    tunnel.drain();
    Ok(tunnel)
}

/// [`start`] for `machine`'s console: a program that isn't installed is `tool_missing`, another
/// failure to start it `no_console` saying why.
fn started(machine: &str, spec: &ProcessSpec, local: SocketAddr, via: &str, ledger: &Path) -> Result<Tunnel> {
    start(spec, local, via, ledger).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Error::ToolMissing {
            step: STEP.to_owned(),
            tool: spec.program.clone(),
        },
        _ => no_console(machine, format!("its tunnel's `{}` didn't start: {e}", spec.program)),
    })
}

/// Starts `spec` as a tunnel listening at `local` through `via`, in a process group of its own,
/// recorded in `ledger` and stopped when the tunnel is dropped; its standard error kept for
/// [`Tunnel::why_closed`].
#[expect(
    clippy::disallowed_methods,
    reason = "the SSH tunnel is a long-lived child its `Tunnel` owns and stops, not a call the process module waits on"
)]
fn start(spec: &ProcessSpec, local: SocketAddr, via: &str, ledger: &Path) -> std::io::Result<Tunnel> {
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
    own_group(&mut command);
    let ssh = command.spawn()?;
    let recorded = ssh.id().and_then(|pid| Recorded::tunnel(ledger, pid));
    Ok(Tunnel {
        local,
        via: via.to_owned(),
        ssh,
        drain: None,
        _recorded: recorded,
    })
}

/// The tunnel's `ssh` leads a process group of its own (what the ledger ends), and gets no
/// signal meant for the viewer's terminal (its Ctrl-C ends the viewer, which ends the tunnel).
#[cfg(unix)]
fn own_group(command: &mut tokio::process::Command) {
    command.process_group(0);
}

/// No console window for the tunnel's `ssh` on Windows, as for every tool the runner starts: from
/// a GUI app it would get one of its own, open as long as the tunnel, and closing it ends `ssh`.
#[cfg(windows)]
fn own_group(command: &mut tokio::process::Command) {
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    command.creation_flags(CREATE_NO_WINDOW.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to(host: &str) -> To {
        To {
            via: Login::new("admin", "203.0.113.7").unwrap(),
            host: host.to_owned(),
            port: 3389,
            key: None,
        }
    }

    #[test]
    fn the_forward_listens_on_the_loopback_and_checks_the_pinned_keys() {
        use crate::process::fake::CallView as _;
        let dir = tempfile::tempdir().unwrap();
        let spec = ssh_spec("dc", &to("10.9.0.20"), Path::new("/k/id"), dir.path(), 40000).unwrap();
        assert_eq!(spec.flag("-L"), Some("127.0.0.1:40000:10.9.0.20:3389"), "{:?}", spec.args);
        for never_waits in ["StrictHostKeyChecking=yes", "BatchMode=yes", "ConnectTimeout=10"] {
            assert!(spec.has_run(&["-o", never_waits]), "{never_waits}: {:?}", spec.args);
        }
        assert!(!spec.args.iter().any(|a| a.ends_with("=accept-new")), "{:?}", spec.args);
        assert_eq!((spec.flag("-F"), spec.flag("-i")), (Some("none"), Some("/k/id")), "{:?}", spec.args);
        assert_eq!(spec.args.last().map(String::as_str), Some("admin@203.0.113.7"));
        let refused = ssh_spec("dc", &to("-oProxyCommand=x"), Path::new("/k"), dir.path(), 1).unwrap_err();
        assert_eq!(refused.code(), crate::ErrorCode::NoConsole, "an option, never an address: {refused}");
    }

    #[test]
    fn an_ipv6_host_is_bracketed_in_the_forward() {
        assert_eq!(bracketed("fd00::5"), "[fd00::5]");
        assert_eq!(bracketed("10.0.1.5"), "10.0.1.5");
    }

    #[test]
    fn a_missing_ssh_is_a_tool_missing() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ProcessSpec::new("isothrum-no-such-ssh", ["-N"]);
        let err = started("dc", &spec, "127.0.0.1:1".parse().unwrap(), "a@b", dir.path()).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::ToolMissing, "{err}");
    }

    /// A tunnel of `script` run by `sh` (a stand-in `ssh`), recorded in a ledger under `dir`.
    #[cfg(unix)]
    fn stand_in(script: &str, dir: &Path) -> Tunnel {
        let spec = ProcessSpec::new("sh", ["-c", script]);
        start(&spec, "127.0.0.1:1".parse().unwrap(), "admin@203.0.113.7", dir).unwrap()
    }

    /// Regression: `ssh`'s standard error went to null, so a tunnel that didn't open never said
    /// why (a host key that changed, a key refused).
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tunnel_that_closed_says_the_last_line_its_ssh_said() {
        let dir = tempfile::tempdir().unwrap();
        let mut refused = stand_in("echo 'debug1: x' >&2; echo 'Host key verification failed.' >&2; exit 255", dir.path());
        assert_eq!(refused.why_closed().await.as_deref(), Some("Host key verification failed."));
        assert_eq!(stand_in("exit 255", dir.path()).why_closed().await, None, "nothing said, nothing made up");
    }

    /// Regression (#126): nothing said when a tunnel's `ssh` ended, so `isothrum screen` kept
    /// saying a dead tunnel was open.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_tunnel_says_when_its_ssh_ended() {
        let dir = tempfile::tempdir().unwrap();
        let mut tunnel = stand_in("exit 255", dir.path());
        tunnel.drain();
        let said = tokio::time::timeout(Duration::from_secs(10), tunnel.closed()).await.expect("seen closing");
        assert_eq!(said, "its ssh exited with code 255");
    }
}
