//! Which Docker-compatible engine this machine's CLI talks to, the others that also answer, and
//! switching or starting one the way the player would. Also tells a stopped daemon apart from one
//! that refuses this account, as a [`DockerDenied`] code the UI renders in the player's language.
//!
//! Everything runs through the no-unsafe [`ProcessRunner`]: starting an engine opens its app
//! (`open -a`, `cmd /C start`) or runs its CLI (`colima start`, `pkexec systemctl start docker`),
//! all ordinary process spawns, so no elevation injection point is needed here (unlike the VM
//! self-test's Hyper-V boot).

use std::time::Duration;

use isothrum_runner::ProcessRunner;
use isothrum_runner::process::ProcessError;
use serde::Serialize;

use crate::exec;
use crate::{Error, Result};

/// How long each engine's context may take to answer before it counts as stopped.
const ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
/// How long a started engine is waited for: 90 tries, 2 s apart (Docker Desktop and OrbStack take
/// a while on first start).
const START_TRIES: u32 = 90;
const START_POLL: Duration = Duration::from_secs(2);

/// Without pkexec there is no graphical password prompt (Debian ships it apart from polkit).
const NO_PKEXEC: &str = "pkexec isn't installed, so your password can't be asked for. Install it once in a terminal (sudo apt install pkexec), then try again.";

/// Why a running Docker daemon refused this account, as a code the UI renders in the player's
/// language. Its [`Display`](std::fmt::Display) is the plain-English fallback.
#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum DockerDenied {
    /// Unix: the daemon's socket refused this account (not in the `docker` group, or not since its
    /// last login).
    SocketRefused,
    /// Windows: the engine's named pipe refused this account (not in the `docker-users` group).
    PipeAccessDenied,
}

impl std::fmt::Display for DockerDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            DockerDenied::SocketRefused => {
                "Docker is running but your account can't use it yet. Add your user to the \"docker\" group (`sudo usermod -aG docker $USER`), then log out and back in."
            }
            DockerDenied::PipeAccessDenied => {
                "Docker is running but your account can't use it yet. Add your user to the \"docker-users\" group, then sign out and back in."
            }
        };
        f.write_str(msg)
    }
}

/// `docker info`'s operating-system line: the daemon answers (for this account).
///
/// # Errors
///
/// The tool's own failure: the daemon is down, or up but refusing this account (see
/// [`access_denied`]).
pub(super) async fn daemon_os(runner: &dyn ProcessRunner) -> Result<String> {
    exec::run_read(
        runner,
        "docker",
        &["info", "--format", "{{.OperatingSystem}}"],
        None,
    )
    .await
}

/// Names the running engine from the daemon's OS string and the active CLI context.
pub(super) async fn engine_in_use(runner: &dyn ProcessRunner, os: &str) -> &'static str {
    let context = exec::read(runner, "docker", &["context", "show"], None)
        .await
        .unwrap_or_default();
    engine_named(os, &context)
}

/// The engine a daemon's OS string and the CLI's current context point to.
fn engine_named(os: &str, context: &str) -> &'static str {
    let (os, context) = (os.to_lowercase(), context.to_lowercase());
    if os.contains("orbstack") || context.contains("orbstack") {
        "orbstack"
    } else if context.contains("colima") {
        "colima"
    } else if os.contains("rancher") || context.contains("rancher") {
        "rancher-desktop"
    } else if os.contains("podman") || context.contains("podman") {
        "podman"
    } else if os.contains("docker desktop") || context.starts_with("desktop-") {
        "docker-desktop"
    } else {
        "docker-engine"
    }
}

/// The engine behind a Docker CLI context, by the names the engines give their contexts.
/// `default` only counts on Linux, where it is the native daemon; elsewhere its socket is a link to
/// whichever desktop app installed it, so it says nothing on its own.
fn context_engine(name: &str, os: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n == "orbstack" {
        Some("orbstack")
    } else if n.starts_with("desktop-") {
        Some("docker-desktop")
    } else if n == "colima" || n.starts_with("colima-") {
        Some("colima")
    } else if n == "rancher-desktop" {
        Some("rancher-desktop")
    } else if n.starts_with("podman") {
        Some("podman")
    } else if n == "default" && os == "linux" {
        Some("docker-engine")
    } else {
        None
    }
}

/// The Docker CLI contexts, each with the engine behind it (unknown contexts are left out).
async fn engine_contexts(runner: &dyn ProcessRunner) -> Vec<(String, &'static str)> {
    let out = exec::read(
        runner,
        "docker",
        &["context", "ls", "--format", "{{.Name}}"],
        None,
    )
    .await
    .unwrap_or_default();
    contexts_from(&out, std::env::consts::OS)
}

/// `docker context ls` names (the current one marked ` *`), each with its engine.
fn contexts_from(listing: &str, os: &str) -> Vec<(String, &'static str)> {
    listing
        .lines()
        .map(|l| l.trim().trim_end_matches(" *").to_owned())
        .filter_map(|name| context_engine(&name, os).map(|e| (name, e)))
        .collect()
}

/// Engines whose context answers, probed one by one (a stopped engine fails fast; the timeout
/// covers one that hangs).
pub(super) async fn running_engines(runner: &dyn ProcessRunner) -> Vec<&'static str> {
    let mut running = Vec::new();
    for (name, engine) in engine_contexts(runner).await {
        let args = [
            "--context",
            name.as_str(),
            "version",
            "--format",
            "{{.Server.Version}}",
        ];
        let probe = exec::run(runner, "docker", &args, None);
        if exec::within("docker version", ENGINE_PROBE_TIMEOUT, probe)
            .await
            .is_ok()
            && !running.contains(&engine)
        {
            running.push(engine);
        }
    }
    running
}

/// Which refusal a failed `docker` call carries, `None` when it is not a refusal (the daemon is
/// simply down). Derived from the error's stderr, so the daemon's own words decide the code; both
/// the system report's hint and [`docker_start_engine`] read it.
pub(super) fn denied_reason_of(err: &Error) -> Option<DockerDenied> {
    match err {
        Error::Exit { reason, .. } => denied_reason(reason),
        _ => None,
    }
}

/// The specific refusal `stderr` is: the socket's `permission denied` on unix, or the named pipe's
/// `access is denied` on Windows (`open //./pipe/dockerDesktopLinuxEngine: Access is denied.`).
fn denied_reason(stderr: &str) -> Option<DockerDenied> {
    let s = stderr.to_lowercase();
    if s.contains("permission denied")
        && (s.contains("docker.sock") || s.contains("docker api") || s.contains("docker daemon"))
    {
        Some(DockerDenied::SocketRefused)
    } else if s.contains("access is denied") && s.contains("pipe") {
        Some(DockerDenied::PipeAccessDenied)
    } else {
        None
    }
}

/// Points the Docker CLI at another running engine (`docker context use`), as the player would in a
/// terminal. Labs then run on it.
///
/// # Errors
///
/// [`Error::Invalid`] when no context maps to `engine`, or the switch command's own failure.
pub async fn docker_use_engine(runner: &dyn ProcessRunner, engine: &str) -> Result<()> {
    let name = engine_contexts(runner)
        .await
        .into_iter()
        .find(|(_, e)| *e == engine)
        .map(|(name, _)| name)
        .ok_or_else(|| Error::Invalid(format!("no Docker context for {engine}")))?;
    exec::run(runner, "docker", &["context", "use", &name], None).await?;
    Ok(())
}

/// Starts an installed engine that isn't running, the way the player would (opens its app or runs
/// its CLI), points Docker at it and waits until it answers.
///
/// # Errors
///
/// [`Error::DockerDenied`] when the daemon is up but refuses this account (starting it again can't
/// help), [`Error::Invalid`] when this machine can't start that engine or it never answered, or
/// the launch command's own failure.
pub async fn docker_start_engine(runner: &dyn ProcessRunner, engine: &str) -> Result<()> {
    // Running already, but not for this account: starting it again can't help.
    if let Err(e) = daemon_os(runner).await
        && let Some(denied) = denied_reason_of(&e)
    {
        return Err(Error::DockerDenied(denied));
    }
    launch(runner, engine).await?;
    for _ in 0..START_TRIES {
        // The engine's context appears once it is up; point Docker at it, then ask the daemon.
        let _discard = docker_use_engine(runner, engine).await;
        match daemon_os(runner).await {
            Ok(_) => return Ok(()),
            // Up now, but not for this account: say so instead of waiting out the 3 minutes.
            Err(e) => {
                if let Some(denied) = denied_reason_of(&e) {
                    return Err(Error::DockerDenied(denied));
                }
            }
        }
        tokio::time::sleep(START_POLL).await;
    }
    Err(Error::Invalid(format!(
        "{engine} didn't answer within 3 minutes. Check its window, then re-check."
    )))
}

/// Opens the engine's app, or runs its CLI, on this OS.
async fn launch(runner: &dyn ProcessRunner, engine: &str) -> Result<String> {
    match (std::env::consts::OS, engine) {
        ("macos", "docker-desktop") => exec::run(runner, "open", &["-a", "Docker"], None).await,
        ("macos", "orbstack") => exec::run(runner, "open", &["-a", "OrbStack"], None).await,
        ("windows", "docker-desktop") => {
            let exe = r"C:\Program Files\Docker\Docker\Docker Desktop.exe";
            exec::run(runner, "cmd", &["/C", "start", "", exe], None).await
        }
        ("linux", "docker-engine") => {
            exec::run(runner, "pkexec", &["systemctl", "start", "docker"], None)
                .await
                .map_err(no_pkexec)
        }
        (_, "colima") => exec::run(runner, "colima", &["start"], None).await,
        _ => Err(Error::Invalid(format!(
            "can't start {engine} here automatically; start it yourself, then re-check."
        ))),
    }
}

/// Maps a missing pkexec to the plain-language hint (no graphical password prompt without it);
/// every other error passes through.
fn no_pkexec(e: Error) -> Error {
    match e {
        Error::Process(ProcessError::Missing { .. }) => Error::Invalid(NO_PKEXEC.to_owned()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    #[test]
    fn names_the_engine_from_the_daemon_and_the_context() {
        assert_eq!(engine_named("OrbStack", "default"), "orbstack");
        assert_eq!(engine_named("Ubuntu 24.04", "colima"), "colima");
        assert_eq!(
            engine_named("Rancher Desktop WSL Distribution", "default"),
            "rancher-desktop"
        );
        assert_eq!(engine_named("Docker Desktop", "default"), "docker-desktop");
        assert_eq!(engine_named("Debian", "desktop-linux"), "docker-desktop");
        assert_eq!(engine_named("Ubuntu 24.04 LTS", "default"), "docker-engine");
        assert_eq!(engine_named("podman", "default"), "podman");
    }

    #[test]
    fn contexts_map_to_engines_and_default_only_counts_on_linux() {
        let listing = "default *\ndesktop-linux\norbstack\ncolima-dev\nmy-remote\n";
        assert_eq!(
            contexts_from(listing, "macos"),
            [
                ("desktop-linux".to_owned(), "docker-desktop"),
                ("orbstack".to_owned(), "orbstack"),
                ("colima-dev".to_owned(), "colima")
            ]
        );
        assert_eq!(
            contexts_from("default *\n", "linux"),
            [("default".to_owned(), "docker-engine")]
        );
        assert_eq!(
            context_engine("podman-machine-default", "macos"),
            Some("podman")
        );
        assert_eq!(context_engine("default", "macos"), None);
    }

    #[tokio::test]
    async fn only_the_engines_whose_context_answers_are_running() {
        let fake = FakeProcesses::new(|call| {
            let line = call.line();
            if line.contains("context ls") {
                Reply::ok("orbstack\ncolima\n")
            } else if line.contains("--context colima") {
                Reply::fail(1, "Cannot connect to the Docker daemon")
            } else {
                Reply::ok("27.0.0")
            }
        });
        assert_eq!(running_engines(&fake).await, ["orbstack"]);
    }

    #[tokio::test]
    async fn the_engine_in_use_comes_from_the_daemon_os_and_the_active_context() {
        let fake = FakeProcesses::rules(&[("context show", Reply::ok("colima"))]);
        assert_eq!(engine_in_use(&fake, "Ubuntu 24.04").await, "colima");
    }

    #[tokio::test]
    async fn a_denied_daemon_is_told_apart_from_a_stopped_one() {
        let down = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(
                1,
                "Cannot connect to the Docker daemon at unix:///var/run/docker.sock",
            ),
        )]);
        assert!(denied_reason_of(&daemon_os(&down).await.unwrap_err()).is_none());

        let denied = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(
                1,
                "permission denied while trying to connect to the Docker daemon socket",
            ),
        )]);
        assert_eq!(
            denied_reason_of(&daemon_os(&denied).await.unwrap_err()),
            Some(DockerDenied::SocketRefused)
        );
    }

    #[test]
    fn the_denial_code_is_derived_from_the_socket_and_the_pipe_refusals() {
        assert_eq!(
            denied_reason(
                "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock"
            ),
            Some(DockerDenied::SocketRefused)
        );
        assert_eq!(
            denied_reason("open //./pipe/dockerDesktopLinuxEngine: Access is denied."),
            Some(DockerDenied::PipeAccessDenied)
        );
        assert_eq!(
            denied_reason("Cannot connect to the Docker daemon. Is the docker daemon running?"),
            None
        );
        // Each code renders a plain-text hint and a short serde code for the UI to localise.
        assert_ne!(DockerDenied::SocketRefused.to_string(), "");
        assert_eq!(
            serde_json::to_value(DockerDenied::PipeAccessDenied).unwrap(),
            "pipe-access-denied"
        );
    }

    #[tokio::test]
    async fn docker_use_switches_the_cli_to_the_engines_context() {
        let fake =
            FakeProcesses::rules(&[("context ls", Reply::ok("default *\norbstack\ncolima-dev\n"))]);
        docker_use_engine(&fake, "colima").await.unwrap();
        assert!(
            fake.lines()
                .iter()
                .any(|l| l.contains("context use colima-dev")),
            "{:?}",
            fake.lines()
        );
        let err = docker_use_engine(&fake, "podman").await.unwrap_err();
        assert_eq!(err.to_string(), "no Docker context for podman");
    }

    #[tokio::test(start_paused = true)]
    async fn docker_start_launches_the_engine_points_at_it_and_waits_for_its_daemon() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);
        let fake = FakeProcesses::new(move |call| {
            let line = call.line();
            if line.contains("colima start") {
                flag.store(true, Ordering::SeqCst);
                Reply::ok("")
            } else if line.contains("docker info") {
                if flag.load(Ordering::SeqCst) {
                    Reply::ok("Ubuntu 24.04 LTS")
                } else {
                    Reply::fail(
                        1,
                        "Cannot connect to the Docker daemon. Is the docker daemon running?",
                    )
                }
            } else if line.contains("context ls") {
                Reply::ok(if flag.load(Ordering::SeqCst) {
                    "default *\ncolima\n"
                } else {
                    "default *\n"
                })
            } else {
                Reply::ok("")
            }
        });
        docker_start_engine(&fake, "colima").await.unwrap();
        assert!(started.load(Ordering::SeqCst), "colima was started");
        assert!(
            fake.lines()
                .iter()
                .any(|l| l.contains("context use colima"))
        );
    }

    #[tokio::test]
    async fn docker_start_reports_a_denied_daemon_without_starting_anything() {
        let fake = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(
                1,
                "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock",
            ),
        )]);
        let err = docker_start_engine(&fake, "colima").await.unwrap_err();
        assert!(
            matches!(err, Error::DockerDenied(DockerDenied::SocketRefused)),
            "{err:?}"
        );
        assert!(!fake.lines().iter().any(|l| l.contains("colima start")));
    }

    #[tokio::test]
    async fn an_engine_this_machine_cannot_start_is_refused() {
        let fake = FakeProcesses::rules(&[(
            "docker info",
            Reply::fail(
                1,
                "Cannot connect to the Docker daemon. Is the docker daemon running?",
            ),
        )]);
        let err = docker_start_engine(&fake, "some-engine").await.unwrap_err();
        assert!(err.to_string().contains("can't start some-engine"), "{err}");
    }
}
