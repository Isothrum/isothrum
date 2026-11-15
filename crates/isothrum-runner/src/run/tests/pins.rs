//! A Compose run's published host ports stay the run's across a park and a resume, though
//! Docker gives an ephemeral publish a new port each time its container starts: those of the
//! services it starts, and those of a service under a profile it attached (an attack box).

use std::collections::BTreeMap;

use super::*;
use crate::testing::modelled;

/// Each service's container port.
const SERVICES: [(&str, u16); 2] = [("web", 80), ("attackbox", 7681)];

/// What the engine holds: each service's container, running or not, with the host port it
/// published at its last start, and how many starts it saw.
#[derive(Debug, Default)]
struct Engine {
    containers: BTreeMap<String, (bool, u16)>,
    starts: u16,
}

/// The host port the run's override (an `-f` file named `compose-pins.yaml`) gives `service`'s
/// port `target`.
fn pinned(call: &Call, service: &str, target: u16) -> Option<u16> {
    let file = call.args.iter().find(|a| a.ends_with("compose-pins.yaml"))?;
    let text = std::fs::read_to_string(file).ok()?;
    let block = text.split(&format!("  \"{service}\":\n")).nth(1)?;
    block
        .lines()
        .take_while(|l| l.starts_with("    "))
        .find_map(|l| l.trim().strip_prefix("- \"")?.strip_suffix(&format!(":{target}/tcp\""))?.parse().ok())
}

/// Docker with Compose: `web` and `attackbox` (under a profile) each publish one port with no
/// host port, which gets a new ephemeral one at each start unless the override pins it.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    let rest = match args.as_slice() {
        ["compose", "version", "--short"] => return Reply::ok("2.29.7"),
        ["compose", "-p", _, rest @ ..] => rest,
        _ => return Reply::ok(""),
    };
    let words: Vec<&str> = rest
        .iter()
        .copied()
        .filter(|a| !std::path::Path::new(a).is_absolute())
        .filter(|a| *a != "-f")
        .collect();
    let named: Vec<&str> = words.iter().copied().filter(|w| SERVICES.iter().any(|(s, _)| s == w)).collect();
    match words.first().copied() {
        Some("up") => {
            for service in if named.is_empty() { vec!["web"] } else { named } {
                let target = SERVICES.iter().find(|(s, _)| *s == service).map_or(0, |(_, t)| *t);
                e.starts += 1;
                let host = pinned(call, service, target).unwrap_or(32_767 + e.starts);
                e.containers.insert(service.to_owned(), (true, host));
            }
            Reply::ok("")
        }
        Some("stop") => {
            for (running, _) in e.containers.values_mut() {
                *running = false;
            }
            Reply::ok("")
        }
        Some("rm") => {
            for service in named {
                e.containers.remove(service);
            }
            Reply::ok("")
        }
        Some("ps") => {
            let lines: Vec<String> = e
                .containers
                .iter()
                .map(|(service, (running, host))| {
                    let target = SERVICES.iter().find(|(s, _)| s == service).map_or(0, |(_, t)| *t);
                    let (state, publishers) = if *running {
                        ("running", format!(r#"[{{"PublishedPort":{host},"TargetPort":{target},"Protocol":"tcp"}}]"#))
                    } else {
                        ("exited", "[]".to_owned())
                    };
                    format!(r#"{{"Service":"{service}","State":"{state}","Health":"","ExitCode":0,"Publishers":{publishers},"Labels":""}}"#)
                })
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        Some("config") if words.get(1) == Some(&"--services") => Reply::ok("web\n"),
        Some("config") if named.contains(&"attackbox") => Reply::ok(r#"{"services": {"attackbox": {"ports": [{"target": 7681, "protocol": "tcp"}]}}}"#),
        Some("config") => Reply::ok(r#"{"services": {"web": {"ports": [{"target": 80, "protocol": "tcp"}]}}}"#),
        _ => Reply::ok(""),
    }
}

/// A Compose project whose `web` publishes `"80"`, and a runner on the modelled engine with free
/// host ports from 41001, and its processes.
pub(super) fn compose_lab() -> (tempfile::TempDir, std::path::PathBuf, Runner, Arc<crate::process::fake::FakeProcesses>) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("lab");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("compose.yaml"), "services:\n  web:\n    image: nginx\n    ports: [\"80\"]\n").expect("a compose file");
    let (_engine, fake) = modelled::<Engine>(answer);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(crate::net::FakePorts::new(41001..41010));
    let config = runner_config(&root.path().join("home"), processes(&fake), ports, crate::runner::this_host());
    (root, project, Runner::new(config).expect("a runner"), fake)
}

/// `machine`'s published host port, as the run's status says; none when it publishes none.
async fn port_of(run: &crate::Running<'_>, machine: &str) -> Option<u16> {
    let status = run.status().await.expect("status");
    let m = status.machines.iter().find(|m| m.name == machine)?;
    m.published.first().map(|p| p.host_port.get())
}

#[tokio::test]
async fn a_park_and_a_resume_keep_the_runs_host_ports_an_attached_services_too() {
    let (_root, project, runner, _fake) = compose_lab();
    let run = runner.open(&project, None).expect("opened");

    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    run.attach("attackbox", &RunOptions::new(), &Recorder::new()).await.expect("attached");
    let first = (port_of(&run, "web").await, port_of(&run, "attackbox").await);
    assert_eq!(first, (Some(41001), Some(41002)), "pinned to the ports the runner picked");
    for mode in [crate::Park::Shutdown, crate::Park::Pause] {
        run.park(mode, &RunOptions::new(), &Recorder::new()).await.expect("parked");
        run.resume(&RunOptions::new(), &Recorder::new()).await.expect("resumed");
        let again = (port_of(&run, "web").await, port_of(&run, "attackbox").await);
        assert_eq!(again, first, "the same ports after {mode:?} and a resume");
    }
    run.update(&RunOptions::new(), &Recorder::new()).await.expect("updated");
    assert_eq!(port_of(&run, "web").await, Some(41001), "and after an update");
    run.retire("attackbox", &RunOptions::new(), &Recorder::new()).await.expect("retired");
    let override_file = runner.store.run_dir_path(run.name()).join("compose-pins.yaml");
    let left = std::fs::read_to_string(&override_file).expect("web's pin is left");
    assert!(!left.contains("attackbox"), "the attack box's pin went with it: {left}");
}
