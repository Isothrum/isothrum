//! Scripted end-to-end runs: the `web-portal` manifest through a fake Compose engine (every
//! `docker` call answered from a small model of a project's containers), fake host ports and a
//! fixed clock. No real tool ever runs.

mod access;
mod cancel;
mod checks;
mod cloud;
mod disruptions;
mod docker_vm;
mod event_log;
mod graphical;
mod leaks;
mod lifecycle;
mod ready;
mod refusals;
mod risks;
mod seen;
mod terraform;
mod timeouts;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::net::FakePorts;
use crate::process::fake::{Call, FakeProcesses, Reply};
use crate::testing::{RunFolder, WEB_PORTAL, modelled, processes, runner_config};
use crate::{Event, EventKind, Recorder, RunOptions, Runner, SecretValue, Secrets};

/// The secret the tests deliver.
const SECRET: &str = "token-fake-0001";

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
    /// The `END` line the check runners print instead of their own, if any.
    end_line: Option<&'static str>,
    /// What the `isoloom-check` runner prints instead of its own lines, if anything.
    check_said: Option<&'static str>,
    /// The networks labelled as the project's (`docker network ls`).
    networks: Vec<String>,
    /// Whether `docker compose config` prints something that isn't its JSON.
    unreadable_config: bool,
    /// Whether `docker compose ps` prints something that isn't its JSON while containers exist.
    unreadable_ps: bool,
    /// Whether web's set-up job (`isoloom-init-web-1`) exits non-zero.
    failing_job: bool,
    /// The calls that fail, by a text their line contains.
    failing: Vec<&'static str>,
    /// Where the project's images are.
    images: Images,
}

/// Whether the project's images are on this computer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Images {
    /// Not yet: whatever starts first downloads them.
    #[default]
    Missing,
    /// Downloaded.
    Here,
}

/// A test's runner and its fakes.
struct Fixture {
    folder: RunFolder,
    manifest: PathBuf,
    fake: Arc<FakeProcesses>,
    ports: Arc<FakePorts>,
    engine: Arc<Mutex<Engine>>,
    runner: Runner,
}

fn fixture() -> Fixture {
    fixture_with(WEB_PORTAL)
}

fn fixture_with(manifest: &str) -> Fixture {
    fixture_answering(manifest, answer)
}

/// [`fixture_with`], its engine answering with `answer`.
fn fixture_answering(manifest: &str, answer: fn(&Mutex<Engine>, &Call) -> Reply) -> Fixture {
    fixture_of(manifest, answer, [41873, 41874, 41875])
}

/// [`fixture_with`], the host ports `free` the only free ones.
fn fixture_with_ports(manifest: &str, free: impl IntoIterator<Item = u16>) -> Fixture {
    fixture_of(manifest, answer, free)
}

fn fixture_of(manifest: &str, answer: fn(&Mutex<Engine>, &Call) -> Reply, free: impl IntoIterator<Item = u16>) -> Fixture {
    let folder = RunFolder::new("docker", manifest);
    let (engine, fake) = modelled(answer);
    let ports = Arc::new(FakePorts::new(free));
    let host_ports: Arc<dyn crate::HostPorts> = Arc::<FakePorts>::clone(&ports);
    let runner = Runner::new(runner_config(folder.home.path(), processes(&fake), host_ports, crate::runner::this_host())).unwrap();
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

/// The options the tests start with: the secret given.
fn opts() -> RunOptions {
    let mut secrets = Secrets::new();
    secrets.insert("DEMO_SECRET", SecretValue::new(SECRET));
    RunOptions {
        secrets,
        ..RunOptions::default()
    }
}

/// Every event's kind, in order.
fn kinds(events: &Recorder) -> Vec<EventKind> {
    events.events().into_iter().map(|e: Event| e.kind).collect()
}

/// A runner on `f`'s home whose Compose `up` answers `up`.
fn whose_up_answers(f: &Fixture, up: Reply) -> Runner {
    let mut config = f.runner.config.clone();
    config.processes = Arc::new(FakeProcesses::rules(&[
        (" config --format json", Reply::ok(r#"{"services":{}}"#)),
        (" up --detach ", up),
    ]));
    Runner::new(config).unwrap()
}

/// The fake engine's answer to `call`.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let line = call.line();
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let up = e.containers == Containers::Up;
    if line.contains(" ps --all ") || line.contains(" ps --format json") {
        if e.unreadable_ps && e.containers != Containers::None {
            return Reply::ok("not json");
        }
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
        return Reply::ok(&format!(" Container web  Started\nseeding {}", call.env("DEMO_SECRET").unwrap_or("no secret")));
    }
    if e.failing_job && line.contains("--exit-code-from isoloom-init-web-1") {
        return Reply::fail(1, "set-up failed");
    }
    if let Some(text) = e.failing.iter().find(|t| line.contains(**t)) {
        return Reply::fail(1, &format!("{text} failed"));
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
    let mut lines: Vec<String> = ["web", "db", "isoloom-routes-web", "isoloom-routes-db", "isoloom-probe-amd64"]
        .iter()
        .map(|s| {
            format!(
                r#"{{"Service":"{s}","State":"{state}","Health":"","Publishers":[{}]}}"#,
                if *s == "web" { publishers.as_str() } else { "" }
            )
        })
        .collect();
    if all {
        lines.push(r#"{"Service":"isoloom-init-web-1","State":"exited","ExitCode":0}"#.into());
    }
    if !running && !all {
        return String::new();
    }
    lines.join("\n")
}

fn config(published: Option<u16>) -> String {
    let published = published.map_or_else(|| "\"\"".to_owned(), |p| format!("\"{p}\""));
    format!(
        r#"{{"services":{{"web":{{"ports":[{{"target":80,"host_ip":"127.0.0.1","published":{published},"protocol":"tcp"}}]}},"db":{{}}}},"networks":{{"lab":{{"name":"web-portal_lab"}}}}}}"#
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
    out.push(
        e.end_line
            .map_or_else(|| format!("isoloom-check: END {} passed, 0 failed", names.len()), str::to_owned),
    );
    let time = e.check_time.iter().find(|(s, _)| s == service).map_or(Duration::ZERO, |(_, t)| *t);
    let said = e
        .check_said
        .filter(|_| service == "isoloom-check")
        .map_or_else(|| out.join("\n"), str::to_owned);
    Reply::ok(&said).after(time)
}

#[test]
fn an_elevated_session_is_handed_what_the_runner_read_of_its_environment_and_its_path() {
    let folder = RunFolder::new("docker", WEB_PORTAL);
    let mut config = runner_config(
        folder.home.path(),
        processes(&Arc::new(FakeProcesses::rules(&[]))),
        crate::testing::no_ports(),
        crate::runner::this_host(),
    );
    config.host_env = crate::host_env::HostEnv::of([("HTTPS_PROXY", "http://proxy.corp:3128"), ("VAGRANT_HOME", "D:\\boxes")]);
    config.path = Some("C:\\HashiCorp\\Vagrant\\bin".into());
    let runner = Runner::new(config).unwrap();
    let run = runner.open(&folder.manifest, None).unwrap();
    let handed = run.elevated_env();
    assert_eq!(
        handed,
        [
            ("HTTPS_PROXY".to_owned(), "http://proxy.corp:3128".to_owned()),
            ("VAGRANT_HOME".to_owned(), "D:\\boxes".to_owned()),
            ("PATH".to_owned(), "C:\\HashiCorp\\Vagrant\\bin".to_owned()),
        ]
    );
}
