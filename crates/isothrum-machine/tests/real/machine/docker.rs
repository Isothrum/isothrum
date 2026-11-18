//! This computer's container engines as Docker's CLI reaches them, and a start of one that runs
//! already. No engine is started or stopped: an engine that doesn't answer is the user's, and is
//! left as it is.

use std::sync::{Arc, Mutex, PoisonError};

use isothrum_machine::docker;
use isothrum_machine::install::Elevation;
use isothrum_machine::{Context, Host};
use isothrum_runner::{BoxFuture, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, SystemProcesses};

use crate::{answer, here};

/// This computer's tools, each call recorded (`program args`).
#[derive(Default)]
struct Watched {
    system: SystemProcesses,
    ran: Mutex<Vec<String>>,
}

impl ProcessRunner for Watched {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        let call = std::iter::once(spec.program.as_str())
            .chain(spec.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        self.ran.lock().unwrap_or_else(PoisonError::into_inner).push(call);
        self.system.run(spec, on_line)
    }
}

/// The engine the CLI points at answers, and it is among the engines found, read as running; so
/// is each engine whose own context answers.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_engines_here_are_found_as_they_run() {
    let cx = here().await;
    if answer(&cx, "docker", &["--version"]).await.is_none() {
        skip!("Docker's CLI isn't installed here")
    }
    let engines = docker::engines(&cx).await;
    println!("{engines:#?}");
    let os = match docker::daemon_os(&cx).await {
        Ok(os) => os,
        Err(why) => skip!("no engine answers the CLI here: {why}"),
    };
    let in_use = docker::engine_in_use(&cx, &os).await;
    let found = engines.iter().find(|s| s.engine == in_use);
    assert!(
        found.is_some_and(|s| s.running),
        "the engine in use ({in_use:?}, {os}) is found running: {engines:?}"
    );
    for s in &engines {
        let answers = docker::daemon_info(&cx, "{{.OperatingSystem}}", Some(&s.context)).await.is_ok();
        assert_eq!(s.running, answers, "{s:?}");
    }
}

/// A start of the engine the CLI points at, which runs already, changes nothing: its daemon
/// answers, so no step of its start runs (no app opened, no service started), and the start
/// reports it ready. Only that engine is asked: a start of another points the CLI at it.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn starting_an_engine_that_runs_already_starts_nothing_and_reports_it_ready() {
    let watched = Arc::new(Watched::default());
    let host = Host::here(watched.as_ref()).await;
    let cx = Context::new(Arc::<Watched>::clone(&watched), host);
    if answer(&cx, "docker", &["--version"]).await.is_none() {
        skip!("Docker's CLI isn't installed here")
    }
    let engines = docker::engines(&cx).await;
    let current = engines.iter().find(|s| s.current && s.running).map(|s| s.engine);
    let Some(engine) = current.filter(|e| docker::start_steps(cx.host.os, *e, unattended()).is_some()) else {
        skip!("no engine a setup starts runs as the CLI's current one here: {engines:?}")
    };
    watched.ran.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let mut lines = Vec::new();
    let started = docker::start_engine(&cx, engine, unattended(), &mut |l| lines.push(l.to_owned())).await;
    let ran = watched.ran.lock().unwrap_or_else(PoisonError::into_inner).clone();
    println!("{engine:?}: {started:?}\nran: {ran:#?}\nsaid: {lines:#?}");
    assert_eq!(started, Ok(()));
    assert!(ran.iter().all(|call| call.starts_with("docker ")), "only Docker's CLI was asked: {ran:?}");
    assert!(docker::running_of(&docker::engines(&cx).await).contains(&engine), "{engine:?} still runs");
}

/// What a test says while Docker Desktop's agreement waits for a person.
const AGREEMENT_WAITS: &str =
    "Docker Desktop waits for a person to accept Docker's Subscription Service Agreement in its own window, once on this computer: nothing accepts it for them";

/// Whether Docker Desktop is installed here: its app (macOS), its program (Windows).
fn docker_desktop_installed(cx: &isothrum_machine::Context) -> bool {
    match cx.host.os {
        isothrum_machine::HostOs::Macos => cx.host.devices.applications.join("Docker.app").is_dir(),
        isothrum_machine::HostOs::Windows => {
            std::env::var_os("ProgramFiles").is_some_and(|p| std::path::Path::new(&p).join("Docker").join("Docker").join("Docker Desktop.exe").is_file())
        }
        isothrum_machine::HostOs::Linux => false,
    }
}

/// Docker Desktop installed here answers once its user has accepted Docker's agreement in its
/// window, which no install accepts for them: until a person has, once on this computer, the test
/// waits (a state of its own, not a failure); accepted, its engine answers when it runs. It
/// starts nothing: a Docker Desktop not running is left as it is.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn docker_desktop_answers_once_its_user_has_accepted_its_agreement() {
    let cx = here().await;
    if !docker_desktop_installed(&cx) {
        skip!("Docker Desktop isn't installed here")
    }
    let accepted = docker::agreement_accepted(&cx);
    println!(
        "Docker Desktop's agreement accepted: {accepted:?} (its settings: {:?})",
        cx.host.devices.docker_desktop_settings
    );
    if accepted != Some(true) {
        waiting!("{AGREEMENT_WAITS}")
    }
    let engines = docker::engines(&cx).await;
    let Some(desktop) = engines.iter().find(|s| s.engine == docker::Engine::DockerDesktop) else {
        skip!("Docker Desktop has no context here yet: {engines:?}")
    };
    if !desktop.running {
        skip!("Docker Desktop isn't running here (a test starts nothing it didn't start)")
    }
    let os = docker::daemon_info(&cx, "{{.OperatingSystem}}", Some(&desktop.context)).await;
    assert!(os.is_ok(), "Docker Desktop's engine answers: {os:?}");
}

/// A start as an unattended setup runs it: as root through `sudo -n` when not root already.
fn unattended() -> Elevation {
    Elevation::Unattended { root: false }
}
