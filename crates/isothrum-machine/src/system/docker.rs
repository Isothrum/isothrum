//! Which Docker-compatible engine this machine's CLI talks to, and the others that also answer.
//! Detection only — naming the running engine for the system report, and telling a stopped daemon
//! apart from one that refuses this account. Starting or switching an engine is the app shell's job
//! and is a follow-up port.

use std::time::Duration;

use isothrum_runner::ProcessRunner;

use crate::Result;
use crate::exec;

/// How long each engine's context may take to answer before it counts as stopped.
const ENGINE_PROBE_TIMEOUT: Duration = Duration::from_secs(4);

/// `docker info`'s operating-system line: the daemon answers (for this account).
///
/// # Errors
///
/// The tool's own failure: the daemon is down, or up but refusing this account (see
/// [`access_denied`]).
pub(super) async fn daemon_os(runner: &dyn ProcessRunner) -> Result<String> {
    exec::run_read(runner, "docker", &["info", "--format", "{{.OperatingSystem}}"], None).await
}

/// Names the running engine from the daemon's OS string and the active CLI context.
pub(super) async fn engine_in_use(runner: &dyn ProcessRunner, os: &str) -> &'static str {
    let context = exec::read(runner, "docker", &["context", "show"], None).await.unwrap_or_default();
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
    let out = exec::read(runner, "docker", &["context", "ls", "--format", "{{.Name}}"], None).await.unwrap_or_default();
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
        let args = ["--context", name.as_str(), "version", "--format", "{{.Server.Version}}"];
        let probe = exec::run(runner, "docker", &args, None);
        if exec::within("docker version", ENGINE_PROBE_TIMEOUT, probe).await.is_ok() && !running.contains(&engine) {
            running.push(engine);
        }
    }
    running
}

/// Whether `err` is Docker running but refusing this account (not the daemon being down): the
/// socket's `permission denied` on unix, or the named pipe's `access is denied` on Windows.
pub(super) fn access_denied(err: &crate::Error) -> bool {
    matches!(err, crate::Error::Exit { reason, .. } if denied_text(reason))
}

/// The socket refusal on unix (`permission denied … docker.sock`), or the named pipe's on Windows
/// (`open //./pipe/dockerDesktopLinuxEngine: Access is denied.`).
fn denied_text(stderr: &str) -> bool {
    let s = stderr.to_lowercase();
    let socket = s.contains("permission denied") && (s.contains("docker.sock") || s.contains("docker api") || s.contains("docker daemon"));
    let pipe = s.contains("access is denied") && s.contains("pipe");
    socket || pipe
}

/// What the player can do about a daemon that is up but refusing their account.
pub(super) fn denied_hint() -> String {
    if cfg!(windows) {
        "Docker is running but your account can't use it yet. Add your user to the \"docker-users\" group, then sign out and back in.".to_owned()
    } else {
        "Docker is running but your account can't use it yet. Add your user to the \"docker\" group (`sudo usermod -aG docker $USER`), then log out and back in.".to_owned()
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
        assert_eq!(engine_named("Rancher Desktop WSL Distribution", "default"), "rancher-desktop");
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
            [("desktop-linux".to_owned(), "docker-desktop"), ("orbstack".to_owned(), "orbstack"), ("colima-dev".to_owned(), "colima")]
        );
        assert_eq!(contexts_from("default *\n", "linux"), [("default".to_owned(), "docker-engine")]);
        assert_eq!(context_engine("podman-machine-default", "macos"), Some("podman"));
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
        let down = FakeProcesses::rules(&[("docker info", Reply::fail(1, "Cannot connect to the Docker daemon at unix:///var/run/docker.sock"))]);
        assert!(!access_denied(&daemon_os(&down).await.unwrap_err()));

        let denied = FakeProcesses::rules(&[("docker info", Reply::fail(1, "permission denied while trying to connect to the Docker daemon socket"))]);
        assert!(access_denied(&daemon_os(&denied).await.unwrap_err()));
    }

    #[test]
    fn the_denial_text_matches_the_socket_and_the_pipe_refusals() {
        assert!(denied_text("permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock"));
        assert!(denied_text("open //./pipe/dockerDesktopLinuxEngine: Access is denied."));
        assert!(!denied_text("Cannot connect to the Docker daemon. Is the docker daemon running?"));
        assert_ne!(denied_hint(), "");
    }
}
