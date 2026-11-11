//! What a user does to a run outside the runner, through a Compose engine modelled container by
//! container: a container removed (`docker rm -f`) or stopped (`docker stop`) while the run starts
//! or runs, the Docker daemon stopped, Docker uninstalled, the process running an operation killed
//! (SIGKILL, a crash, the computer off) so its record is left mid-way. Each test says what the run
//! must be after it: its status true to what is there, a refusal or failure that says why, and a
//! way back (a `down`, or the next `up` clearing what is left) that leaves nothing behind.

mod cases;
mod sequences;

use std::collections::BTreeMap;

// The scripted runs' own: the fakes, the manifest's folder, and how they pin and configure ports.
use super::*;
use crate::RunState;

/// The project's long-running services: its two machines, then Isoloom's plumbing.
const SERVICES: [&str; 5] = ["web", "db", "isoloom-routes-web", "isoloom-routes-db", "isoloom-probe-amd64"];
/// Its machines.
const MEMBERS: [&str; 2] = ["web", "db"];
/// Its set-up job.
const JOB: &str = "isoloom-init-web-1";
/// What every `docker` call prints while the daemon is stopped.
const DAEMON_DOWN: &str = "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?";

/// A container's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctr {
    Running,
    Exited,
}

/// One Compose project, container by container, and what the user does to it.
#[derive(Debug, Default)]
struct Project {
    /// Each service's container, by service; a service with none isn't there.
    ctrs: BTreeMap<&'static str, Ctr>,
    /// The Docker daemon is stopped: every `docker` call fails.
    daemon_down: bool,
    /// Docker is uninstalled: no `docker` to run.
    uninstalled: bool,
    /// What the user does while the next `compose up --wait` waits for the services.
    during_up: Option<Disruption>,
}

/// What the user does to the project while a start waits for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disruption {
    /// `docker rm -f` the container of this service.
    Remove(&'static str),
    /// Stops the Docker daemon.
    StopDaemon,
}

impl Project {
    fn state(&self, service: &str) -> Option<Ctr> {
        self.ctrs.get(service).copied()
    }
}

/// The modelled engine's answer to `call`.
fn answer(engine: &Mutex<Project>, call: &Call) -> Reply {
    let mut p = engine.lock().unwrap_or_else(PoisonError::into_inner);
    if call.program == "docker" && p.uninstalled {
        return Reply::missing();
    }
    if call.program == "docker" && p.daemon_down {
        return Reply::fail(1, DAEMON_DOWN);
    }
    let line = call.line();
    if line.contains(" ps --all ") || line.contains(" ps --format json") {
        return Reply::ok(&ps(&p, line.contains("--all"), pinned(call)));
    }
    if line.contains(" config --format json") {
        return Reply::ok(&config(pinned(call)));
    }
    if line.contains(" up --detach ") {
        for s in SERVICES {
            p.ctrs.insert(s, Ctr::Running);
        }
        return match p.during_up.take() {
            Some(Disruption::Remove(s)) => {
                p.ctrs.remove(s);
                Reply::fail(1, &format!("Error response from daemon: No such container: web-portal-{s}-1"))
            }
            Some(Disruption::StopDaemon) => {
                p.daemon_down = true;
                Reply::fail(1, DAEMON_DOWN)
            }
            None => Reply::ok(" Container web  Started"),
        };
    }
    if line.contains(&format!("--exit-code-from {JOB}")) {
        p.ctrs.insert(JOB, Ctr::Exited);
        return Reply::ok("");
    }
    if line.contains(" down --volumes") {
        p.ctrs.clear();
        return Reply::ok("");
    }
    if line.ends_with(" stop") {
        p.ctrs.values_mut().for_each(|c| *c = Ctr::Exited);
        return Reply::ok("");
    }
    if line.ends_with(" start") {
        for s in SERVICES {
            if let Some(c) = p.ctrs.get_mut(s) {
                *c = Ctr::Running;
            }
        }
        return Reply::ok("");
    }
    if line.starts_with("docker ps -aq") {
        return Reply::ok(&p.ctrs.keys().map(|s| format!("c-{s}")).collect::<Vec<_>>().join("\n"));
    }
    if line.starts_with("docker rm -f") {
        for id in call.args.iter().filter_map(|a| a.strip_prefix("c-")) {
            p.ctrs.retain(|s, _| *s != id);
        }
    }
    Reply::ok("")
}

/// `docker compose ps --format json` (`--all`: the stopped containers too) of the project.
fn ps(project: &Project, all: bool, port: Option<u16>) -> String {
    project
        .ctrs
        .iter()
        .filter(|(_, c)| all || **c == Ctr::Running)
        .map(|(s, c)| {
            let state = if *c == Ctr::Running { "running" } else { "exited" };
            let publishers = match (*s, port) {
                ("web", Some(p)) => format!(r#"{{"URL":"127.0.0.1","PublishedPort":{p},"TargetPort":80,"Protocol":"tcp"}}"#),
                _ => String::new(),
            };
            format!(r#"{{"Service":"{s}","State":"{state}","Health":"","ExitCode":0,"Publishers":[{publishers}]}}"#)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A runner on the modelled project.
struct Fixture {
    folder: RunFolder,
    fake: Arc<FakeProcesses>,
    project: Arc<Mutex<Project>>,
    runner: Runner,
}

fn fixture() -> Fixture {
    let folder = RunFolder::new("docker", WEB_PORTAL);
    let (project, fake) = modelled(answer);
    // The fake hands each free port out once: enough for every start of a long sequence.
    let ports: Arc<dyn crate::HostPorts> = Arc::new(FakePorts::new(41_000..42_000));
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), ports, crate::runner::this_host())).unwrap();
    Fixture { folder, fake, project, runner }
}

impl Fixture {
    fn project(&self) -> std::sync::MutexGuard<'_, Project> {
        self.project.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn run(&self) -> crate::Running<'_> {
        self.runner.open(&self.folder.manifest, None).unwrap()
    }

    /// The run's record, as its last operation left it.
    fn record(&self) -> crate::RunRecord {
        self.runner.store.record(&crate::RunName::new("web-portal").unwrap()).unwrap().unwrap()
    }

    /// The run's record, left in `state` as a process killed mid-operation leaves it.
    fn killed_in(&self, state: RunState) {
        let mut record = self.record();
        record.state = state;
        self.runner.store.save(&record).unwrap();
    }

    /// The run, started, then left `starting` as a process killed mid-start leaves it.
    async fn killed_start(&self) -> crate::Running<'_> {
        let run = self.run();
        run.up(&super::opts(), &Recorder::new()).await.unwrap();
        self.killed_in(RunState::Starting);
        run
    }

    /// The calls whose line contains `text`.
    fn called(&self, text: &str) -> usize {
        self.fake.lines().iter().filter(|l| l.contains(text)).count()
    }

    /// The whole of `run` is up: every member's container runs, and the run reads ready.
    async fn is_whole(&self, run: &crate::Running<'_>) {
        assert!(self.every_member_runs(), "{:?}", self.project().ctrs);
        assert_eq!(run.status().await.unwrap().state, RunState::Ready);
    }

    /// Every member machine's container runs.
    fn every_member_runs(&self) -> bool {
        MEMBERS.iter().all(|m| self.project().state(m) == Some(Ctr::Running))
    }
}
