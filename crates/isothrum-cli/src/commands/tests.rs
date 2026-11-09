//! The commands end to end, on a runner whose tools are scripted (`flag-portal` through a tiny
//! fake Compose engine).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
const FLAG_PORTAL_ACCESS: &str = include_str!("../../../isothrum-runner/tests/fixtures/flag-portal-access.run.json");

/// Where the fake project's containers are.
const NONE: u8 = 0;
const UP: u8 = 1;
const STOPPED: u8 = 2;

struct Setup {
    _home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    fake: Arc<FakeProcesses>,
    term: Term,
    out: Buf,
    err: Buf,
    /// The environment the command reads (`--secret-from-env`).
    env: Vec<(&'static str, &'static str)>,
}

/// A run generated for docker, and a runner whose `docker` is a tiny model of one project.
fn setup() -> Setup {
    setup_with(FLAG_PORTAL)
}

/// [`setup`] with `manifest` as the run's `run.json`.
fn setup_with(manifest: &str) -> Setup {
    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = run.path().join(".isoloom/docker");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.json"), manifest).unwrap();
    let state = Arc::new(AtomicU8::new(NONE));
    let fake = Arc::new(FakeProcesses::new(move |call| {
        let line = call.line();
        let now = state.load(Ordering::SeqCst);
        let web = |s: &str| format!(r#"{{"Service":"web","State":"{s}","Publishers":[{{"PublishedPort":41873,"TargetPort":80,"Protocol":"tcp"}}]}}"#);
        match () {
            () if line.contains(" ps --all ") && now == STOPPED => Reply::ok(&web("exited")),
            () if line.contains(" ps ") && !line.contains(" ps -q ") => Reply::ok(if now == UP { web("running") } else { String::new() }.as_str()),
            () if line.contains(" ps -q attacker") => Reply::ok("c0ffee"),
            () if line.contains("--profile access up") => Reply::ok(&format!("attacker from {}", call.env("ISOLOOM_ACCESS_IMAGE").unwrap_or("its image"))),
            () if line.contains(" up --detach ") => {
                state.store(UP, Ordering::SeqCst);
                Reply::ok(&format!("seeding {}", call.env("CTF_FLAG_MAIN").unwrap_or("no flag")))
            }
            () if line.ends_with(" stop") => {
                state.store(STOPPED, Ordering::SeqCst);
                Reply::ok("")
            }
            () if line.ends_with(" start") => {
                state.store(UP, Ordering::SeqCst);
                Reply::ok("")
            }
            () if line.contains(" down --volumes") => {
                state.store(NONE, Ordering::SeqCst);
                Reply::ok("")
            }
            () if line.contains(" config --format json") => Reply::ok(r#"{"services":{"web":{},"db":{},"attacker":{}}}"#),
            () if line.contains("--profile check run") => Reply::ok("isoloom-check: PASS http://web/\nisoloom-check: END 1 passed, 0 failed"),
            () => Reply::ok(""),
        }
    }));
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::<FakeProcesses>::clone(&fake);
    config.ports = Arc::new(FakePorts::new([41873]));
    let (out, err) = (Buf::default(), Buf::default());
    let term = Term::new(Box::new(out.clone()), Box::new(err.clone()));
    Setup {
        _home: home,
        runner: Runner::new(config).unwrap(),
        run,
        fake,
        term,
        out,
        err,
        env: Vec::new(),
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
        let var = |name: &str| self.env.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned());
        commands::run(&cli, &self.runner, &self.term, &var).await
    }

    /// The last event the command printed (with `--json`).
    fn last_event(&self) -> serde_json::Value {
        serde_json::from_str(self.out.take().lines().last().unwrap()).unwrap()
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
    let last = s.last_event();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("result"), Some("down")));
    assert_eq!(s.run(&["status"]).await.unwrap(), Verdict::Pass);
    assert!(s.out.take().starts_with("flag-portal: absent\n"));
}

#[tokio::test]
async fn a_sensitive_output_shows_as_masked_in_the_status_json_and_text() {
    let s = setup_with(&FLAG_PORTAL.replace(r#""sensitive": false"#, r#""sensitive": true"#));
    assert_eq!(s.run(&["up", "--json"]).await.unwrap(), Verdict::Pass);
    assert!(!s.out.take().contains("127.0.0.1:41873"), "no event says it");

    assert_eq!(s.run(&["status", "--json"]).await.unwrap(), Verdict::Pass);
    let status: serde_json::Value = serde_json::from_str(&s.out.take()).unwrap();
    assert_eq!(
        (&status["outputs"][0]["value"], &status["outputs"][0]["sensitive"]),
        (&"***".into(), &true.into())
    );

    assert_eq!(s.run(&["status"]).await.unwrap(), Verdict::Pass);
    let said = s.out.take();
    assert!(said.contains("\nweb_portal: ***\n") && !said.contains("127.0.0.1:41873"), "{said}");
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
    let dir = s.run.path().join(".isoloom/docker").display().to_string();
    assert!(
        console.starts_with(&format!("cd {dir} && docker compose --project-name flag-portal ")),
        "{console}"
    );
    assert!(console.contains(" exec web sh -c 'if command -v bash"), "its script quoted whole: {console}");
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

#[tokio::test]
async fn a_run_parks_resumes_and_updates() {
    let s = setup();
    assert_eq!(s.run(&["up", "--host-label", "here"]).await.unwrap(), Verdict::Pass);
    s.out.take();

    assert_eq!(s.run(&["park", "--host-label", "elsewhere", "--json"]).await.unwrap(), Verdict::Fail);
    assert_eq!(s.last_event()["code"], "running_elsewhere");
    assert_eq!(s.run(&["park", "--host-label", "here", "--json"]).await.unwrap(), Verdict::Pass);
    let last = s.last_event();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("result"), Some("park")));
    assert_eq!(last["data"]["state"], "parked");

    assert_eq!(s.run(&["resume"]).await.unwrap(), Verdict::Pass);
    assert_eq!(s.out.take(), "The portal answers at http://localhost:41873/.\n");

    assert_eq!(s.run(&["update", "--json"]).await.unwrap(), Verdict::Pass);
    let updated = s.last_event();
    assert_eq!((updated["event"].as_str(), updated["op"].as_str()), (Some("result"), Some("update")));
}

#[tokio::test]
async fn provision_runs_the_set_up_again_and_says_when_there_is_none() {
    let s = setup();
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    s.err.take();
    assert_eq!(s.run(&["provision"]).await.unwrap(), Verdict::Fail);
    assert!(s.err.take().contains("no provision steps"));

    let mut manifest: serde_json::Value = serde_json::from_str(FLAG_PORTAL).unwrap();
    let again = serde_json::json!({ "id": "again", "argv": ["docker", "compose", "-p", "${run.name}", "exec", "-T", "web", "true"], "timeout_s": 30 });
    manifest["lifecycle"]["provision"] = serde_json::json!({ "all": [again], "machine": [] });
    let with_steps = setup_with(&manifest.to_string());
    assert_eq!(with_steps.run(&["up"]).await.unwrap(), Verdict::Pass);
    assert_eq!(with_steps.run(&["provision", "--json"]).await.unwrap(), Verdict::Pass);
    assert_eq!(with_steps.last_event()["op"], "provision");
    assert!(
        with_steps.fake.calls().iter().any(|c| c.line().ends_with(" exec -T web true")),
        "the provision step ran"
    );
}

#[tokio::test]
async fn attach_says_where_the_access_machine_is_and_detach_takes_it_out() {
    let mut s = setup_with(FLAG_PORTAL_ACCESS);
    s.env.push(("MY_FLAG", "flag{from-env}"));
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    s.out.take();
    s.err.take();

    let attached = s
        .run(&["attach", "--network", "lab", "--image", "me/kali", "--secret-from-env", "CTF_FLAG_MAIN=MY_FLAG"])
        .await;
    assert_eq!(attached.unwrap(), Verdict::Pass);
    assert_eq!(s.out.take(), "Attached attacker at 10.30.0.99.\n");
    let up = s.fake.calls().into_iter().rfind(|c| c.line().contains("--profile access up")).unwrap();
    assert_eq!(up.env("ISOLOOM_ACCESS_IMAGE"), Some("me/kali"), "the picked image is delivered");
    assert!(s.err.take().contains("attacker from me/kali"));

    assert_eq!(s.run(&["detach", "--json"]).await.unwrap(), Verdict::Pass);
    let last = s.last_event();
    assert_eq!((last["event"].as_str(), last["op"].as_str()), (Some("result"), Some("detach")));
    assert_eq!(last["data"]["machine"], "attacker");
    assert!(s.fake.calls().iter().any(|c| c.line().contains("--profile access rm -sf attacker")));
}

/// `attach`'s `--param` and `--secret-from-env` reach the operation: a value it was given is
/// masked in what it prints, and a param it refuses fails it.
#[tokio::test]
async fn attach_takes_the_params_and_secrets_it_is_given() {
    let mut s = setup_with(FLAG_PORTAL_ACCESS);
    s.env.push(("MY_FLAG", "me/kali"));
    assert_eq!(s.run(&["up"]).await.unwrap(), Verdict::Pass);
    s.err.take();
    // The image is the secret's value here: what the attach prints of it is masked.
    let attached = s.run(&["attach", "--image", "me/kali", "--secret-from-env", "CTF_FLAG_MAIN=MY_FLAG"]).await;
    assert_eq!(attached.unwrap(), Verdict::Pass);
    let said = s.err.take();
    assert!(said.contains("attacker from ***") && !said.contains("me/kali"), "{said}");

    let fresh = setup_with(FLAG_PORTAL_ACCESS);
    assert_eq!(fresh.run(&["attach", "--param", "ISOLOOM_DERIVED=7"]).await.unwrap(), Verdict::Fail);
    let refused = fresh.err.take();
    assert!(refused.contains("error: ") && refused.contains("ISOLOOM_DERIVED"), "{refused}");
}
