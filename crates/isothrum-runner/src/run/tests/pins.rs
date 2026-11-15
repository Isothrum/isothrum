//! A Compose run's published host ports stay the run's across a park and a resume, though
//! Docker gives an ephemeral publish a new port each time its container starts.

use super::*;
use crate::testing::modelled;

/// What the engine holds of `web`: whether it runs, how many times it started, and the host port
/// it published at its last start.
#[derive(Debug, Default)]
struct Engine {
    running: bool,
    starts: u16,
    host: u16,
}

/// The host port the run's override (an `-f` file named `compose-pins.yaml`) gives web's port 80.
fn pinned(call: &Call) -> Option<u16> {
    let file = call.args.iter().find(|a| a.ends_with("compose-pins.yaml"))?;
    let text = std::fs::read_to_string(file).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("- \"")?.strip_suffix(":80/tcp\"")?.parse().ok())
}

/// Docker with Compose: `web` publishes `"80"`, which gets a new ephemeral port at each start
/// unless an override pins it.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    let rest = match args.as_slice() {
        ["compose", "version", "--short"] => return Reply::ok("2.29.7"),
        ["compose", "-p", _, rest @ ..] => rest,
        _ => return Reply::ok(""),
    };
    let verb = rest
        .iter()
        .position(|a| !a.starts_with('-') && !std::path::Path::new(a).is_absolute())
        .map(|i| &rest[i..]);
    match verb {
        Some(["up", ..]) => {
            e.running = true;
            e.starts += 1;
            e.host = pinned(call).unwrap_or(32_767 + e.starts);
            Reply::ok("")
        }
        Some(["stop", ..]) => {
            e.running = false;
            Reply::ok("")
        }
        Some(["ps", ..]) => {
            let state = if e.running { "running" } else { "exited" };
            let publishers = if e.running {
                format!(r#"[{{"PublishedPort":{},"TargetPort":80,"Protocol":"tcp"}}]"#, e.host)
            } else {
                "[]".to_owned()
            };
            Reply::ok(&format!(
                r#"{{"Service":"web","State":"{state}","Health":"","ExitCode":0,"Publishers":{publishers},"Labels":""}}"#
            ))
        }
        Some(["config", "--services"]) => Reply::ok("web\n"),
        Some(["config", "--format", "json"]) => Reply::ok(r#"{"services": {"web": {"ports": [{"target": 80, "protocol": "tcp"}]}}}"#),
        _ => Reply::ok(""),
    }
}

/// Web's published host port, as the run's status says.
async fn web_port(run: &crate::Running<'_>) -> u16 {
    let status = run.status().await.expect("status");
    let web = status.machines.iter().find(|m| m.name == "web").expect("web");
    web.published.first().map(|p| p.host_port.get()).expect("a published port")
}

#[tokio::test]
async fn a_park_and_a_resume_keep_the_runs_host_ports() {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("lab");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("compose.yaml"), "services:\n  web:\n    image: nginx\n    ports: [\"80\"]\n").expect("a compose file");
    let (_engine, fake) = modelled::<Engine>(answer);
    let ports: Arc<dyn crate::HostPorts> = Arc::new(crate::net::FakePorts::new(41001..41010));
    let config = runner_config(&root.path().join("home"), processes(&fake), ports, crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    let run = runner.open(&project, None).expect("opened");

    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    assert_eq!(web_port(&run).await, 41001, "pinned to the port the runner picked");
    for mode in [crate::Park::Shutdown, crate::Park::Pause] {
        run.park(mode, &RunOptions::new(), &Recorder::new()).await.expect("parked");
        run.resume(&RunOptions::new(), &Recorder::new()).await.expect("resumed");
        assert_eq!(web_port(&run).await, 41001, "the same port after {mode:?} and a resume");
    }
    run.update(&RunOptions::new(), &Recorder::new()).await.expect("updated");
    assert_eq!(web_port(&run).await, 41001, "and after an update");
}
