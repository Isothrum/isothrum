//! The Docker-compatible engines this computer has: each CLI context with an engine behind it,
//! whether its daemon answers, which one the CLI uses, and pointing the CLI at another
//! (`docker context use`). Every call is the Docker CLI's, through the [`Context`]'s runner.
//!
//! A daemon that gives no answer says why ([`NoAnswer`]): no CLI, refused to this account (its
//! socket's or named pipe's permission: joining a group helps, starting the engine doesn't),
//! stopped, or an empty answer. An installed engine that doesn't answer is started as its user
//! would start it ([`start_engine`]).

use std::time::Duration;

use isothrum_runner::{Captured, ProcessSpec};
use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::exec::Unanswered;
use crate::host::HostOs;

mod start;

pub use start::{DESKTOP_BACKEND_GRACE, START_POLL, START_TRIES, StartFailed, start_engine, start_steps};

/// A Docker-compatible engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS), ts(rename = "DockerEngine"))]
#[serde(rename_all = "kebab-case")]
pub enum Engine {
    /// Docker Desktop.
    DockerDesktop,
    /// OrbStack.
    Orbstack,
    /// Colima.
    Colima,
    /// Rancher Desktop.
    RancherDesktop,
    /// Podman.
    Podman,
    /// Docker Engine, the native daemon on Linux.
    #[serde(rename = "docker-engine")]
    Native,
}

impl Engine {
    /// Every engine.
    pub const ALL: [Engine; 6] = [
        Engine::DockerDesktop,
        Engine::Orbstack,
        Engine::Colima,
        Engine::RancherDesktop,
        Engine::Podman,
        Engine::Native,
    ];

    /// Its id, its serde name (`docker-desktop`).
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Engine::DockerDesktop => "docker-desktop",
            Engine::Orbstack => "orbstack",
            Engine::Colima => "colima",
            Engine::RancherDesktop => "rancher-desktop",
            Engine::Podman => "podman",
            Engine::Native => "docker-engine",
        }
    }

    /// The engine with id `id` ([`Engine::id`]).
    #[must_use]
    pub fn from_id(id: &str) -> Option<Engine> {
        Engine::ALL.into_iter().find(|e| e.id() == id)
    }

    /// Its name as its vendor writes it (`Docker Desktop`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Engine::DockerDesktop => "Docker Desktop",
            Engine::Orbstack => "OrbStack",
            Engine::Colima => "Colima",
            Engine::RancherDesktop => "Rancher Desktop",
            Engine::Podman => "Podman",
            Engine::Native => "Docker Engine",
        }
    }

    /// Where its vendor offers it.
    #[must_use]
    pub fn url(self) -> &'static str {
        match self {
            Engine::DockerDesktop => "https://www.docker.com/products/docker-desktop/",
            Engine::Orbstack => "https://orbstack.dev/",
            Engine::Colima => "https://github.com/abiosoft/colima",
            Engine::RancherDesktop => "https://rancherdesktop.io/",
            Engine::Podman => "https://podman.io/",
            Engine::Native => "https://docs.docker.com/engine/install/",
        }
    }
}

/// One Docker-compatible engine found on this computer: the CLI context that reaches it, whether
/// its daemon answers, and whether it is the CLI's current context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    /// Which engine it is.
    pub engine: Engine,
    /// The CLI context that reaches it.
    pub context: String,
    /// Whether its daemon answers.
    pub running: bool,
    /// Whether its context is the CLI's current one.
    pub current: bool,
}

/// Why the Docker daemon gave no answer.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NoAnswer {
    /// The Docker CLI isn't installed.
    NoCli,
    /// The daemon runs but refused this account: its socket (`permission denied … docker.sock`)
    /// or its named pipe (`Access is denied`). What the CLI said.
    Denied(String),
    /// It doesn't answer (stopped, or ran out of the probe's time): why.
    Down(String),
    /// `docker info --format <field>` answered nothing, though the daemon answers.
    Empty(String),
}

impl NoAnswer {
    /// Whether the daemon refused this account ([`NoAnswer::Denied`]).
    #[must_use]
    pub fn denied(&self) -> bool {
        matches!(self, NoAnswer::Denied(_))
    }
}

impl std::fmt::Display for NoAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoAnswer::NoCli => f.write_str("the Docker CLI isn't installed"),
            NoAnswer::Denied(said) | NoAnswer::Down(said) => f.write_str(said),
            NoAnswer::Empty(field) => write!(f, "docker info --format {field} answered nothing, though the Docker daemon answers"),
        }
    }
}

impl std::error::Error for NoAnswer {}

/// Whether `said` (a failed `docker` call's) says the daemon refused this account: the socket's
/// refusal on unix (`permission denied … docker.sock`, `… the docker API`, `… the Docker daemon
/// socket`), or the named pipe's on Windows (`open //./pipe/dockerDesktopLinuxEngine: Access is
/// denied.`).
#[must_use]
pub fn says_denied(said: &str) -> bool {
    let s = said.to_lowercase();
    let socket = s.contains("permission denied") && ["docker.sock", "docker api", "docker daemon"].iter().any(|w| s.contains(w));
    let pipe = s.contains("access is denied") && s.contains("pipe");
    socket || pipe
}

/// `docker [--context <context>] <args>`.
pub(crate) fn docker(context: Option<&str>, args: &[&str]) -> ProcessSpec {
    let on = context.map(|c| ["--context", c]).into_iter().flatten();
    ProcessSpec::new("docker", on.chain(args.iter().copied()))
}

/// Docker Desktop's own `docker` CLI, inside its app bundle (macOS), when it is there: the one a
/// call falls back on while `PATH` has none. Docker Desktop links it onto `PATH` only once its
/// first start's setup is done, and until then its engine, started, is asked through this one.
fn bundled_cli(cx: &Context) -> Option<String> {
    let cli = cx.host.devices.applications.join("Docker.app/Contents/Resources/bin/docker");
    (cx.host.os == HostOs::Macos && cli.is_file()).then(|| cli.display().to_string())
}

/// What `docker args` printed when it exited 0, else why the daemon gave no answer. With no
/// `docker` on `PATH`, Docker Desktop's own CLI is asked, where it is ([`bundled_cli`]).
async fn ask(cx: &Context, limit: Duration, spec: &ProcessSpec) -> Result<String, NoAnswer> {
    let missing = |answer: &Result<Captured, Unanswered>| matches!(answer, Err(Unanswered::NotStarted(isothrum_runner::ProcessError::Missing { .. })));
    let mut answer = cx.probe_within(limit, spec).await;
    if missing(&answer)
        && let Some(cli) = bundled_cli(cx)
    {
        let mut bundled = spec.clone();
        bundled.program = cli;
        answer = cx.probe_within(limit, &bundled).await;
    }
    match answer {
        Ok(captured) => captured.output().map_err(|failed| failure(&captured, failed.reason)),
        Err(Unanswered::NotStarted(isothrum_runner::ProcessError::Missing { .. })) => Err(NoAnswer::NoCli),
        Err(e) => Err(NoAnswer::Down(e.to_string())),
    }
}

/// A failed call's reason as a [`NoAnswer`]: refused to this account, else not answering.
fn failure(captured: &Captured, reason: String) -> NoAnswer {
    let said = captured.stdout.iter().chain(&captured.stderr_tail).cloned().collect::<Vec<_>>().join("\n");
    if says_denied(&said) {
        NoAnswer::Denied(reason)
    } else {
        NoAnswer::Down(reason)
    }
}

/// A daemon's `docker info` answer, when it gave one: Docker 26 and older print an empty line
/// (and exit 0) when the daemon doesn't answer.
fn answered(info: &str) -> Option<&str> {
    Some(info.trim()).filter(|out| !out.is_empty())
}

/// `docker info --format <field>` (`{{.ServerVersion}}`, say), of the daemon `context` reaches
/// (the current one when `None`): the daemon answers this account. Docker 26 and older (Debian's
/// `docker.io`) exit 0 from `docker info` when the daemon doesn't answer, saying so on standard
/// error alone with an empty line on standard output: an empty answer is asked again of `docker
/// version`, which fails with the reason (not running, or refused to this account).
///
/// # Errors
///
/// [`NoAnswer`]: why the daemon gave none.
pub async fn daemon_info(cx: &Context, field: &str, context: Option<&str>) -> Result<String, NoAnswer> {
    let out = ask(cx, cx.probe_timeout, &docker(context, &["info", "--format", field])).await?;
    if answered(&out).is_some() {
        return Ok(out);
    }
    ask(cx, cx.probe_timeout, &docker(context, &["version", "--format", "{{.Server.Os}}"])).await?;
    Err(NoAnswer::Empty(field.to_owned()))
}

/// The current daemon's operating system line (`docker info`'s `{{.OperatingSystem}}`): it
/// answers this account.
///
/// # Errors
///
/// [`NoAnswer`]: why the daemon gave none.
pub async fn daemon_os(cx: &Context) -> Result<String, NoAnswer> {
    daemon_info(cx, "{{.OperatingSystem}}", None).await
}

/// The engine running, from its daemon's operating system line (`daemon_os`) and the CLI's
/// current context.
pub async fn engine_in_use(cx: &Context, daemon_os: &str) -> Engine {
    let context = ask(cx, cx.probe_timeout, &docker(None, &["context", "show"])).await.unwrap_or_default();
    engine_named(daemon_os.trim(), &context)
}

/// The engine a daemon's operating system line and the CLI's current context point to.
fn engine_named(os: &str, context: &str) -> Engine {
    let (os, context) = (os.to_lowercase(), context.to_lowercase());
    if os.contains("orbstack") || context.contains("orbstack") {
        Engine::Orbstack
    } else if context.contains("colima") {
        Engine::Colima
    } else if os.contains("rancher") || context.contains("rancher") {
        Engine::RancherDesktop
    } else if os.contains("podman") || context.contains("podman") {
        Engine::Podman
    } else if os.contains("docker desktop") || context.starts_with("desktop-") {
        Engine::DockerDesktop
    } else {
        Engine::Native
    }
}

/// The engine behind a Docker CLI context, by the names the engines give their contexts.
/// `default` only counts on Linux, where it is the native daemon; elsewhere its socket is a link
/// to whichever desktop app installed it, so it says nothing on its own.
fn context_engine(name: &str, os: HostOs) -> Option<Engine> {
    let n = name.to_lowercase();
    if n == "orbstack" {
        Some(Engine::Orbstack)
    } else if n.starts_with("desktop-") {
        Some(Engine::DockerDesktop)
    } else if n == "colima" || n.starts_with("colima-") {
        Some(Engine::Colima)
    } else if n == "rancher-desktop" {
        Some(Engine::RancherDesktop)
    } else if n.starts_with("podman") {
        Some(Engine::Podman)
    } else if n == "default" && os == HostOs::Linux {
        Some(Engine::Native)
    } else {
        None
    }
}

/// The Docker CLI contexts: each name, and whether it is the current one.
///
/// # Errors
///
/// Why `docker context ls` failed.
async fn contexts(cx: &Context) -> Result<Vec<(String, bool)>, NoAnswer> {
    let listed = ask(cx, cx.probe_timeout, &docker(None, &["context", "ls", "--format", "{{.Name}}\t{{.Current}}"])).await?;
    Ok(contexts_from(&listed))
}

/// `docker context ls` lines: the name, then whether it is current (`true`, or a trailing `*` as
/// the table view marks it).
fn contexts_from(listing: &str) -> Vec<(String, bool)> {
    listing
        .lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next()?.to_owned();
            let current = words.any(|w| w == "true" || w == "*");
            Some((name, current))
        })
        .collect()
}

/// Every engine this computer has, one per context that names one or answers: probed one by one
/// with `docker --context <c> info`, given as long as any probe ([`Context::probe_timeout`], as
/// [`daemon_os`]): a stopped engine fails fast, and one waking up (Docker Desktop's Resource
/// Saver, a cold CLI on Windows) answers within it. A context that answers names its engine from the daemon's operating system
/// too, so `default` on a Mac reads as the desktop app behind it; one that doesn't is known by its
/// name alone. Two contexts to one engine keep the first (the current one first of all). None
/// when the contexts can't be listed (no Docker CLI).
pub async fn engines(cx: &Context) -> Vec<EngineStatus> {
    let mut listed = contexts(cx).await.unwrap_or_default();
    listed.sort_by_key(|(_, current)| !current);
    let mut found: Vec<EngineStatus> = Vec::new();
    for (name, current) in listed {
        let info = docker(Some(&name), &["info", "--format", "{{.OperatingSystem}}"]);
        let answer = ask(cx, cx.probe_timeout, &info).await.ok().and_then(|os| answered(&os).map(str::to_owned));
        let engine = match &answer {
            Some(daemon_os) => engine_named(daemon_os, &name),
            None => match context_engine(&name, cx.host.os) {
                Some(engine) => engine,
                None => continue,
            },
        };
        if !found.iter().any(|f| f.engine == engine) {
            found.push(EngineStatus {
                engine,
                context: name,
                running: answer.is_some(),
                current,
            });
        }
    }
    found
}

/// The engines whose daemon answers.
#[must_use]
pub fn running_of(engines: &[EngineStatus]) -> Vec<Engine> {
    engines.iter().filter(|e| e.running).map(|e| e.engine).collect()
}

/// Why an engine's context couldn't be found or used.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContextError {
    /// The contexts couldn't be listed: why.
    Unlisted(NoAnswer),
    /// No context on this computer names the engine.
    None(Engine),
    /// `docker context use` failed: why.
    Unused(NoAnswer),
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ContextError::Unlisted(why) => write!(f, "the Docker contexts couldn't be listed: {why}"),
            ContextError::None(engine) => write!(f, "no Docker context for {}", engine.id()),
            ContextError::Unused(why) => write!(f, "docker context use failed: {why}"),
        }
    }
}

impl std::error::Error for ContextError {}

/// The Docker context behind `engine` on this computer, to run on it (`docker --context <name>`).
///
/// # Errors
///
/// [`ContextError::Unlisted`] when the contexts can't be listed, [`ContextError::None`] when no
/// context names `engine`.
pub async fn engine_context(cx: &Context, engine: Engine) -> Result<String, ContextError> {
    let listed = contexts(cx).await.map_err(ContextError::Unlisted)?;
    let mut named = listed
        .into_iter()
        .filter_map(|(name, _)| (context_engine(&name, cx.host.os) == Some(engine)).then_some(name));
    named.next().ok_or(ContextError::None(engine))
}

/// Points the Docker CLI at `engine` (`docker context use`), as its user would in a terminal.
///
/// # Errors
///
/// [`engine_context`]'s, or [`ContextError::Unused`] when `docker context use` fails.
pub async fn use_engine(cx: &Context, engine: Engine) -> Result<(), ContextError> {
    let name = engine_context(cx, engine).await?;
    ask(cx, cx.probe_timeout, &docker(None, &["context", "use", &name]))
        .await
        .map_err(ContextError::Unused)?;
    Ok(())
}

#[cfg(test)]
mod tests;
