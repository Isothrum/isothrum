//! Scripted end-to-end runs: the `flag-portal` manifest through a fake Compose engine (every
//! `docker` call answered from a small model of a project's containers), fake host ports and a
//! fixed clock. No real tool ever runs.

mod access;
mod checks;
mod cloud;
mod docker_vm;
mod leaks;
mod lifecycle;
mod risks;
mod terraform;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::secrets::{SecretValue, Secrets};
use crate::testing::{FLAG_PORTAL, LabFolder, modelled, processes, runner_config};
use crate::{Event, EventKind, Recorder, RunOptions, Runner};

/// The flag the tests deliver.
const FLAG: &str = "flag{fake-0001}";

/// Where the project's containers are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Containers {
    #[default]
    None,
    Up,
    Stopped,
}

/// A small model of one Compose project.
#[derive(Debug, Default)]
struct Engine {
    containers: Containers,
    /// How long each check runner takes, by service.
    check_time: Vec<(String, Duration)>,
    /// The networks labelled as the project's (`docker network ls`).
    networks: Vec<String>,
    /// Whether `docker compose config` prints something that isn't its JSON.
    unreadable_config: bool,
}

/// A test's runner and its fakes.
struct Fixture {
    folder: LabFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    ports: Arc<FakePorts>,
    engine: Arc<Mutex<Engine>>,
    runner: Runner,
}

fn fixture() -> Fixture {
    fixture_with(FLAG_PORTAL)
}

fn fixture_with(manifest: &str) -> Fixture {
    fixture_answering(manifest, answer)
}

/// [`fixture_with`], its engine answering with `answer`.
fn fixture_answering(manifest: &str, answer: fn(&Mutex<Engine>, &Call) -> Reply) -> Fixture {
    let folder = LabFolder::new("docker", manifest);
    let (engine, fake) = modelled(answer);
    let ports = Arc::new(FakePorts::new([41873, 41874, 41875]));
    let host_ports: Arc<dyn crate::HostPorts> = Arc::<FakePorts>::clone(&ports);
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), host_ports, crate::this_host())).unwrap();
    Fixture {
        manifest: folder.manifest.clone(),
        folder,
        fake,
        ports,
        engine,
        runner,
    }
}

impl Fixture {
    /// The runner's home: everything it writes.
    fn home(&self) -> &std::path::Path {
        self.folder.home.path()
    }

    fn containers(&self) -> Containers {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner).containers
    }

    fn set(&self, containers: Containers) {
        self.engine.lock().unwrap_or_else(PoisonError::into_inner).containers = containers;
    }

    fn check_takes(&self, service: &str, time: Duration) {
        self.engine
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .check_time
            .push((service.into(), time));
    }

    /// The compose calls whose line contains `text`.
    fn called(&self, text: &str) -> usize {
        self.fake.lines().iter().filter(|l| l.contains(text)).count()
    }
}

/// The options the tests start with: the flag given.
fn opts() -> RunOptions {
    let mut secrets = Secrets::new();
    secrets.insert("CTF_FLAG_MAIN", SecretValue::new(FLAG));
    RunOptions {
        secrets,
        ..RunOptions::default()
    }
}

/// Every event's kind, in order.
fn kinds(events: &Recorder) -> Vec<EventKind> {
    events.events().into_iter().map(|e: Event| e.kind).collect()
}

/// The fake engine's answer to `call`.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let line = call.line();
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let up = e.containers == Containers::Up;
    if line.contains(" ps --all ") || line.contains(" ps --format json") {
        return Reply::ok(&ps(e.containers, line.contains("--all"), pinned(call)));
    }
    if line.contains(" config --format json") {
        if e.unreadable_config {
            return Reply::ok("not json");
        }
        let published = if call.env("ISOLOOM_PUBLISH_FIXED").is_some() {
            Some(8080)
        } else {
            pinned(call)
        };
        return Reply::ok(&config(published));
    }
    if line.contains(" up --detach ") {
        e.containers = Containers::Up;
        return Reply::ok(&format!(" Container web  Started\nseeding {}", call.env("CTF_FLAG_MAIN").unwrap_or("no flag")));
    }
    if line.starts_with("docker network ls ") {
        return Reply::ok(&e.networks.join("\n"));
    }
    if line.contains("--profile check run") {
        return check_runner(&e, &line);
    }
    let next = match () {
        () if line.ends_with(" stop") && up => Containers::Stopped,
        () if line.ends_with(" start") && e.containers == Containers::Stopped => Containers::Up,
        () if line.contains(" down --volumes") => Containers::None,
        () if line.starts_with("docker ps -aq") => {
            return Reply::ok(if e.containers == Containers::None { "" } else { "c1\nc2" });
        }
        () => e.containers,
    };
    e.containers = next;
    Reply::ok("")
}

/// The host port pinned in the call's pinned-ports file, if any.
fn pinned(call: &Call) -> Option<u16> {
    let file = call.args.iter().find(|a| a.ends_with("ports.yml"))?;
    let text = std::fs::read_to_string(file).ok()?;
    let (_, after) = text.split_once("127.0.0.1:")?;
    after.split(':').next()?.parse().ok()
}

fn ps(containers: Containers, all: bool, port: Option<u16>) -> String {
    let (state, running) = match containers {
        Containers::None => return String::new(),
        Containers::Up => ("running", true),
        Containers::Stopped => ("exited", false),
    };
    let publishers = port.map_or_else(String::new, |p| {
        format!(r#"{{"URL":"127.0.0.1","PublishedPort":{p},"TargetPort":80,"Protocol":"tcp"}}"#)
    });
    let mut lines: Vec<String> = ["web", "db", "web-routes", "db-routes", "isoloom-probe-amd64"]
        .iter()
        .map(|s| {
            format!(
                r#"{{"Service":"{s}","State":"{state}","Health":"","Publishers":[{}]}}"#,
                if *s == "web" { publishers.as_str() } else { "" }
            )
        })
        .collect();
    if all {
        lines.push(r#"{"Service":"web-init-1","State":"exited","ExitCode":0}"#.into());
    }
    if !running && !all {
        return String::new();
    }
    lines.join("\n")
}

fn config(published: Option<u16>) -> String {
    let published = published.map_or_else(|| "\"\"".to_owned(), |p| format!("\"{p}\""));
    format!(
        r#"{{"services":{{"web":{{"ports":[{{"target":80,"host_ip":"127.0.0.1","published":{published},"protocol":"tcp"}}]}},"db":{{}}}},"networks":{{"lab":{{"name":"flag-portal_lab"}}}}}}"#
    )
}

/// A check runner: every check of the fixture's runner passes, after its scripted time.
fn check_runner(e: &Engine, line: &str) -> Reply {
    let service = line.rsplit(' ').next().unwrap_or_default();
    let names: &[&str] = match service {
        "isoloom-check" => &["http://web/", "db:3306", "build/check/check.sh"],
        "isoloom-check-db" => &["web:80 from db", "no internet from db"],
        _ => &["db:3306 from web", "no internet from web"],
    };
    let mut out: Vec<String> = names.iter().map(|n| format!("isoloom-check: PASS {n}")).collect();
    out.push(format!("isoloom-check: END {} passed, 0 failed", names.len()));
    let time = e.check_time.iter().find(|(s, _)| s == service).map_or(Duration::ZERO, |(_, t)| *t);
    Reply::ok(&out.join("\n")).after(time)
}
