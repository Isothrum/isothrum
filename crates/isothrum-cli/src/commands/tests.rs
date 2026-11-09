//! The commands end to end, on a runner whose tools are scripted (`flag-portal` through a tiny
//! fake Compose engine).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::Parser;
use isothrum_runner::process::fake::{FakeProcesses, Reply};
use isothrum_runner::{FakePorts, Runner, RunnerConfig};

use crate::args::Cli;
use crate::commands;
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::term::Term;
use crate::testing::Buf;

const FLAG_PORTAL: &str = include_str!("../../../isothrum-runner/tests/fixtures/flag-portal.run.json");

struct Setup {
    _home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    term: Term,
    out: Buf,
    err: Buf,
}

/// A run generated for docker, and a runner whose `docker` is a tiny model of one project.
fn setup() -> Setup {
    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = run.path().join(".isoloom/docker");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.json"), FLAG_PORTAL).unwrap();
    let up = Arc::new(AtomicBool::new(false));
    let fake = FakeProcesses::new(move |call| {
        let line = call.line();
        let web = r#"{"Service":"web","State":"running","Publishers":[{"PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"}]}"#;
        match () {
            () if line.contains(" ps ") => Reply::ok(if up.load(Ordering::SeqCst) { web } else { "" }),
            () if line.contains(" up --detach ") => Reply::ok(&format!("{}", up.swap(true, Ordering::SeqCst))),
            () if line.contains(" down --volumes") => Reply::ok(&format!("{}", up.swap(false, Ordering::SeqCst))),
            () if line.contains(" config --format json") => Reply::ok(r#"{"services":{"web":{},"db":{}}}"#),
            () if line.contains("--profile check run") => Reply::ok("isoloom-check: PASS http://web/\nisoloom-check: END 1 passed, 0 failed"),
            () => Reply::ok(""),
        }
    });
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::new(fake);
    config.ports = Arc::new(FakePorts::new([41873]));
    let (out, err) = (Buf::default(), Buf::default());
    let term = Term::new(Box::new(out.clone()), Box::new(err.clone()));
    Setup {
        _home: home,
        runner: Runner::new(config).unwrap(),
        run,
        term,
        out,
        err,
    }
}

impl Setup {
    /// `isothrum <args> <run>`: the command's verdict.
    async fn run(&self, args: &[&str]) -> Result<Verdict> {
        let run = self.run.path().display().to_string();
        let mut command_line = vec!["isothrum"];
        command_line.extend_from_slice(args);
        command_line.push(&run);
        let cli = Cli::parse_from(command_line);
        commands::run(&cli, &self.runner, &self.term, &|_| None).await
    }
}

#[tokio::test]
async fn a_lab_goes_up_reports_checks_and_goes_down() {
    let s = setup();
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    assert_eq!(s.out.take(), "The portal answers at http://localhost:41873/.\n");
    assert!(s.err.take().contains("==> Starting the lab's containers…"));

    assert_eq!(s.run(&["status", "--json"]).await.unwrap(), Verdict::Pass);
    let status: serde_json::Value = serde_json::from_str(&s.out.take()).unwrap();
    assert_eq!(status["outputs"][0]["value"], "http://127.0.0.1:41873/");

    let checked = s.run(&["check"]).await.unwrap();
    let said = s.out.take();
    // Each of the three runners reports one pass; the fixture's other four checks go unreported.
    assert_eq!(checked, Verdict::Fail, "{said}");
    assert!(said.contains("3 passed, 4 failed"), "{said}");

    assert_eq!(s.run(&["events", "--json"]).await.unwrap(), Verdict::Pass);
    let events: Vec<serde_json::Value> = s.out.take().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(events.iter().filter(|e| e["event"] == "result").count(), 2, "up and check");

    assert_eq!(s.run(&["down", "--json"]).await.unwrap(), Verdict::Pass);
    let last: serde_json::Value = serde_json::from_str(s.out.take().lines().last().unwrap()).unwrap();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("result"), Some("down")));
    assert_eq!(s.run(&["status"]).await.unwrap(), Verdict::Pass);
    assert!(s.out.take().starts_with("flag-portal: absent\n"));
}

#[tokio::test]
async fn a_refused_operation_is_reported_by_its_error_event() {
    let s = setup();
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    s.err.take();
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Fail);
    assert!(s.err.take().contains("error: run `flag-portal` is already running"));
}

#[tokio::test]
async fn a_lab_without_a_manifest_is_a_usage_error() {
    let s = setup();
    std::fs::remove_dir_all(s.run.path().join(".isoloom")).unwrap();
    let err = s.run(&["status"]).await.unwrap_err();
    assert!(matches!(err, Error::Usage(_)), "{err}");
}

#[tokio::test]
async fn a_console_into_a_machine_is_printed_and_a_machine_is_retired() {
    let s = setup();
    assert_eq!(s.run(&["console", "web"]).await.unwrap(), Verdict::Pass);
    let console = s.out.take();
    assert!(console.starts_with("docker compose --project-name flag-portal "), "{console}");
    assert!(console.contains(" exec web sh -c "), "{console}");
    assert!(s.run(&["console", "nope"]).await.is_err(), "a machine the run doesn't have");

    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    s.out.take();
    assert_eq!(s.run(&["retire", "web", "--json"]).await.unwrap(), Verdict::Pass);
    let last: serde_json::Value = serde_json::from_str(s.out.take().lines().last().unwrap()).unwrap();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("result"), Some("retire")));
    assert_eq!(last["data"]["machine"], "web");
}

/// Retiring a machine the run doesn't have fails through the operation's own `error` event, so
/// the command says why rather than exiting silently.
#[tokio::test]
async fn retiring_an_unknown_machine_says_why() {
    let s = setup();
    assert_eq!(s.run(&["retire", "nope"]).await.unwrap(), Verdict::Fail);
    let said = s.err.take();
    assert!(said.contains("error: ") && said.contains("nope"), "{said}");

    assert_eq!(s.run(&["retire", "nope", "--json"]).await.unwrap(), Verdict::Fail);
    let last: serde_json::Value = serde_json::from_str(s.out.take().lines().last().unwrap()).unwrap();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("error"), Some("retire")));
    assert_eq!(last["code"], "machine_unknown", "{last}");
}
