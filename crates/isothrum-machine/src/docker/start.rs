//! Starting an installed engine that doesn't answer ([`crate::setup::Action::StartEngine`]), the
//! way its user would: its app opened (`open -a`, Docker Desktop's program on Windows), its CLI
//! run (`colima start`), or Docker Engine's service started as root (`systemctl start docker`,
//! through pkexec's prompt or unattended). Then the CLI is pointed at it and its daemon asked
//! until it answers: Docker Desktop and OrbStack take a while on a first start.

use std::time::Duration;

use super::{Engine, NoAnswer, daemon_info, daemon_os, engine_context, use_engine};
use crate::context::Context;
use crate::host::HostOs;
use crate::install::Elevation;
use crate::install::{Step, StepFailed, run_steps, step};

/// How many times a started engine is asked whether it answers, [`START_POLL`] apart: three
/// minutes in all.
pub const START_TRIES: u32 = 90;

/// How long between two of those asks.
pub const START_POLL: Duration = Duration::from_secs(2);

/// Docker Desktop's program on Windows, under Program Files, opened by PowerShell's
/// `Start-Process` (the shell's own launch): the app inherits none of the runner's pipes, so the
/// step ends once the app is opened, not when it quits.
const DOCKER_DESKTOP_WINDOWS: &str = "Start-Process -FilePath (Join-Path $env:ProgramFiles 'Docker\\Docker\\Docker Desktop.exe')";

/// The steps that start `engine` on `os`, root steps elevated by `elevation` (Linux): `None` when
/// it isn't one a setup starts there (its user starts it).
#[must_use]
pub fn start_steps(os: HostOs, engine: Engine, elevation: Elevation) -> Option<Vec<Step>> {
    let one = match (os, engine) {
        (HostOs::Macos, Engine::DockerDesktop) => step("open", &["-a", "Docker"]),
        (HostOs::Macos, Engine::Orbstack) => step("open", &["-a", "OrbStack"]),
        (HostOs::Windows, Engine::DockerDesktop) => step("powershell", &["-NoProfile", "-NonInteractive", "-Command", DOCKER_DESKTOP_WINDOWS]),
        (HostOs::Linux, Engine::Native) => {
            let (program, args) = elevation.as_root("systemctl", &["start".to_owned(), "docker".to_owned()]);
            step(program, &args.iter().map(String::as_str).collect::<Vec<_>>())
        }
        (HostOs::Macos | HostOs::Linux, Engine::Colima) => step("colima", &["start"]),
        _ => return None,
    };
    Some(vec![one])
}

/// Why an engine wasn't started.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StartFailed {
    /// It isn't one a setup starts on this OS: its user starts it.
    NotHere(Engine),
    /// The daemon runs, but refuses this account (its socket's or pipe's permission): starting
    /// it can't help, joining its group (and a new login) does. What the CLI said.
    Denied(String),
    /// A start step failed.
    Step(StepFailed),
    /// Started, it gave no answer within [`START_TRIES`] asks: why the last one failed.
    NoAnswer {
        /// The engine.
        engine: Engine,
        /// How long it was waited for.
        waited: Duration,
        /// The last ask's reason.
        last: String,
    },
}

impl std::fmt::Display for StartFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartFailed::NotHere(engine) => write!(f, "{} isn't started by a setup here; start it yourself", engine.label()),
            StartFailed::Denied(said) => write!(f, "the Docker daemon refuses this account: {said}"),
            StartFailed::Step(failed) => failed.fmt(f),
            StartFailed::NoAnswer { engine, waited, last } => {
                write!(f, "{} gave no answer within {} s of its start: {last}", engine.label(), waited.as_secs())
            }
        }
    }
}

impl std::error::Error for StartFailed {}

/// Starts `engine` ([`start_steps`], each line its steps print handed to `on_line`), points the
/// Docker CLI at it once its context appears, and waits until its daemon answers this account.
/// An engine that runs already (its own context answers) is only pointed at: no step runs.
///
/// # Errors
///
/// [`StartFailed`]: why it wasn't started, or gave no answer.
pub async fn start_engine(cx: &Context, engine: Engine, elevation: Elevation, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), StartFailed> {
    let steps = start_steps(cx.host.os, engine, elevation).ok_or(StartFailed::NotHere(engine))?;
    // Running already, but not for this account: starting it again can't help.
    if let Err(NoAnswer::Denied(said)) = daemon_os(cx).await {
        return Err(StartFailed::Denied(said));
    }
    // Running already: nothing is started again (its app isn't brought up, its service isn't
    // asked to start as root); the CLI is pointed at it, as after a start.
    if answers(cx, engine).await {
        drop(use_engine(cx, engine).await);
        return Ok(());
    }
    run_steps(cx, &steps, on_line).await.map_err(StartFailed::Step)?;
    let mut last = String::new();
    for _ in 0..START_TRIES {
        // Its context appears once it is up: Docker is pointed at it, then its daemon asked.
        // Not up yet, it has no context to use: its daemon's answer below says how it is.
        drop(use_engine(cx, engine).await);
        match daemon_os(cx).await {
            Ok(_) => return Ok(()),
            Err(NoAnswer::Denied(said)) => return Err(StartFailed::Denied(said)),
            Err(why) => last = why.to_string(),
        }
        tokio::time::sleep(START_POLL).await;
    }
    Err(StartFailed::NoAnswer {
        engine,
        waited: START_POLL * START_TRIES,
        last,
    })
}

/// Whether `engine`'s own Docker context answers: it runs already.
async fn answers(cx: &Context, engine: Engine) -> bool {
    match engine_context(cx, engine).await {
        Ok(context) => daemon_info(cx, "{{.OperatingSystem}}", Some(&context)).await.is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;
    use crate::testing::linux_host;

    fn cx(os: HostOs, fake: FakeProcesses) -> Context {
        let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
        host.os = os;
        Context::new(Arc::new(fake), host)
    }

    fn lines(steps: &[Step]) -> Vec<String> {
        steps.iter().map(crate::install::command_line).collect()
    }

    #[test]
    fn each_engine_starts_the_way_its_user_would_on_its_os() {
        let prompt = Elevation::Prompt;
        let started = |os, engine| start_steps(os, engine, prompt).map(|s| lines(&s));
        assert_eq!(started(HostOs::Macos, Engine::DockerDesktop), Some(vec!["$ open -a Docker".to_owned()]));
        assert_eq!(started(HostOs::Macos, Engine::Orbstack), Some(vec!["$ open -a OrbStack".to_owned()]));
        assert_eq!(started(HostOs::Linux, Engine::Colima), Some(vec!["$ colima start".to_owned()]));
        assert_eq!(started(HostOs::Linux, Engine::Native), Some(vec!["$ pkexec systemctl start docker".to_owned()]));
        let windows = started(HostOs::Windows, Engine::DockerDesktop).unwrap();
        assert!(
            windows[0].starts_with("$ powershell -NoProfile -NonInteractive -Command Start-Process -FilePath"),
            "{windows:?}"
        );
        assert!(windows[0].contains("Docker Desktop.exe"), "{windows:?}");
        assert_eq!(started(HostOs::Windows, Engine::Colima), None);
        assert_eq!(started(HostOs::Macos, Engine::Native), None);
        assert_eq!(started(HostOs::Linux, Engine::Podman), None);
    }

    #[test]
    fn docker_engine_starts_unattended_as_root_or_through_sudo() {
        let root = start_steps(HostOs::Linux, Engine::Native, Elevation::Unattended { root: true }).unwrap();
        assert_eq!(lines(&root), ["$ systemctl start docker"]);
        let sudo = start_steps(HostOs::Linux, Engine::Native, Elevation::Unattended { root: false }).unwrap();
        assert_eq!(lines(&sudo), ["$ sudo -n systemctl start docker"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_started_engine_is_used_and_waited_for_until_it_answers() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
        // OrbStack answers from its third ask on, its context listed once it is up.
        let (asked, opened) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicBool::new(false)));
        let (asks, open) = (Arc::clone(&asked), Arc::clone(&opened));
        let fake = Arc::new(FakeProcesses::model(move |call| match call.line().as_str() {
            "open -a OrbStack" => {
                open.store(true, Ordering::SeqCst);
                Reply::ok("")
            }
            "docker info --format '{{.OperatingSystem}}'" if asks.fetch_add(1, Ordering::SeqCst) >= 2 => Reply::ok("OrbStack"),
            "docker info --format '{{.OperatingSystem}}'" => Reply::fail(1, "Cannot connect to the Docker daemon"),
            line if line.starts_with("docker context ls") => Reply::ok("default\ttrue\norbstack\tfalse"),
            "docker context use orbstack" => Reply::ok("orbstack"),
            other => Reply::fail(1, &format!("unexpected {other}")),
        }));
        let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
        host.os = HostOs::Macos;
        let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
        let mut said = Vec::new();
        start_engine(&Context::new(runner, host), Engine::Orbstack, Elevation::Prompt, &mut |l| {
            said.push(l.to_owned());
        })
        .await
        .unwrap();
        assert_eq!(said, ["$ open -a OrbStack"]);
        assert!(opened.load(Ordering::SeqCst));
        assert_eq!(asked.load(Ordering::SeqCst), 3);
        assert_eq!(fake.made("docker context use orbstack").len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_runs_already_is_used_and_not_started_again() {
        // Docker Desktop answers on its own context, the CLI pointed at another: no `open`.
        let fake = FakeProcesses::script()
            .expect("docker info --format {{.OperatingSystem}}", Reply::ok("OrbStack"))
            .expect("docker context ls", Reply::ok("orbstack\ttrue\ndesktop-linux\tfalse"))
            .expect("docker --context desktop-linux info --format {{.OperatingSystem}}", Reply::ok("Docker Desktop"))
            .expect("docker context use desktop-linux", Reply::ok("desktop-linux"))
            .build();
        let mut said = Vec::new();
        let started = start_engine(&cx(HostOs::Macos, fake), Engine::DockerDesktop, Elevation::Prompt, &mut |l| {
            said.push(l.to_owned());
        })
        .await;
        assert_eq!((started, said), (Ok(()), Vec::<String>::new()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_daemon_refusing_this_account_is_not_started_again() {
        let fake = FakeProcesses::script()
            .expect(
                "docker info --format {{.OperatingSystem}}",
                Reply::fail(
                    1,
                    "permission denied while trying to connect to the Docker daemon socket at unix:///var/run/docker.sock",
                ),
            )
            .build();
        let err = start_engine(&cx(HostOs::Linux, fake), Engine::Native, Elevation::Prompt, &mut |_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, StartFailed::Denied(_)), "{err:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_not_started_here_and_a_failed_step_say_so() {
        let none = start_engine(
            &cx(HostOs::Windows, FakeProcesses::script().build()),
            Engine::Podman,
            Elevation::Prompt,
            &mut |_| {},
        )
        .await;
        assert_eq!(none, Err(StartFailed::NotHere(Engine::Podman)));

        let fake = FakeProcesses::script()
            .expect(
                "docker info --format {{.OperatingSystem}}",
                Reply::fail(1, "Cannot connect to the Docker daemon"),
            )
            .expect("docker context ls", Reply::ok("default\ttrue"))
            .expect(
                "docker --context default info --format {{.OperatingSystem}}",
                Reply::fail(1, "Cannot connect to the Docker daemon"),
            )
            .expect("pkexec systemctl start docker", Reply::missing())
            .build();
        let err = start_engine(&cx(HostOs::Linux, fake), Engine::Native, Elevation::Prompt, &mut |_| {}).await;
        assert_eq!(err, Err(StartFailed::Step(StepFailed::NoPkexec)));
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_never_answers_is_given_up_on_with_the_last_reason() {
        let fake = FakeProcesses::model(|call| {
            if call.program == "colima" {
                Reply::ok("")
            } else {
                Reply::fail(1, "Cannot connect to the Docker daemon")
            }
        });
        let err = start_engine(&cx(HostOs::Macos, fake), Engine::Colima, Elevation::Prompt, &mut |_| {})
            .await
            .unwrap_err();
        let StartFailed::NoAnswer { engine, waited, last } = err else {
            panic!("{err:?}")
        };
        assert_eq!((engine, waited), (Engine::Colima, Duration::from_secs(180)));
        assert!(last.contains("Cannot connect"), "{last}");
    }
}
