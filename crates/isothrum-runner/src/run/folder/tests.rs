use std::sync::{Arc, Mutex, PoisonError};

use crate::events::{EventKind, Recorder};
use crate::process::fake::{Call, Reply};
use crate::state::RunState;
use crate::status::MachineState;
use crate::store::ResourceState;
use crate::testing::{modelled, no_ports, processes, runner_config};
use crate::{ErrorCode, Format, RunName, RunOptions, Runner};

/// What a modelled Docker engine holds of a Compose project.
#[derive(Debug, Default)]
struct Engine {
    up: bool,
    paused: bool,
}

/// The engine's answer to `call`.
fn answer(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
    let line = call.line();
    let has = |words: &str| line.contains(words);
    if has("compose version") {
        Reply::ok("2.29.7")
    } else if has(" up ") {
        e.up = true;
        e.paused = false;
        Reply::ok("")
    } else if has(" down ") {
        e.up = false;
        Reply::ok("")
    } else if has(" pause") {
        e.paused = true;
        Reply::ok("")
    } else if has(" ps ") {
        let state = if e.paused { "paused" } else { "running" };
        let listed = format!(
            r#"{{"Service":"web","State":"{state}","Health":"healthy","ExitCode":0,"Publishers":[{{"PublishedPort":41001,"TargetPort":80,"Protocol":"tcp"}}],"Labels":""}}"#
        );
        Reply::ok(if e.up { &listed } else { "" })
    } else if has("container ls") {
        Reply::ok(if e.up { "c1\n" } else { "" })
    } else if has("network ls") {
        Reply::ok(if e.up { "n1\n" } else { "" })
    } else {
        Reply::ok("")
    }
}

/// A Compose project in a temporary folder named `my-Lab`, and a runner with a temporary home on
/// the modelled engine.
fn setup() -> (tempfile::TempDir, std::path::PathBuf, Runner, Arc<Mutex<Engine>>) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("my-Lab");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("compose.yaml"), "services:\n  web:\n    image: nginx\n").expect("a compose file");
    let (engine, fake) = modelled::<Engine>(answer);
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    (root, project, runner, engine)
}

#[tokio::test]
async fn a_compose_folder_comes_up_reads_ready_and_goes_down_with_its_resources_recorded() {
    let (_root, project, runner, engine) = setup();
    let run = runner.open_folder(&project, None, None).expect("opened");
    assert_eq!(run.name().as_str(), "my-lab", "named after its folder");
    assert_eq!(run.project().format, Format::Compose);

    let events = Recorder::new();
    run.up(&RunOptions::new(), &events).await.expect("up");
    let results: Vec<_> = events
        .events()
        .into_iter()
        .filter(|e| matches!(e.kind, EventKind::Result { ok: true, .. }))
        .collect();
    assert_eq!(results.len(), 1, "exactly one result");

    let status = run.status().await.expect("status");
    assert_eq!(status.state, RunState::Ready);
    let web = status.machines.iter().find(|m| m.name == "web").expect("web");
    assert_eq!(web.state, MachineState::Running);
    assert_eq!((web.published[0].port, web.published[0].host_port), (80, 41001));

    let name = RunName::new("my-lab").expect("a name");
    let live = runner.store.db().read(|tx| tx.live_resources(&name)).expect("rows");
    let ids: Vec<_> = live.iter().map(|r| (r.key.as_str(), r.state, r.native_id.as_deref())).collect();
    assert_eq!(
        ids,
        [
            ("container:c1", ResourceState::Created, Some("c1")),
            ("network:n1", ResourceState::Created, Some("n1"))
        ]
    );

    run.down(&RunOptions::new(), &Recorder::new()).await.expect("down");
    assert!(!engine.lock().unwrap_or_else(PoisonError::into_inner).up);
    assert_eq!(run.status().await.expect("status").state, RunState::Absent);
    let left = runner.store.db().read(|tx| tx.live_resources(&name)).expect("rows");
    assert!(left.is_empty(), "every row gone: {left:?}");
}

#[tokio::test]
async fn a_parked_run_reads_parked_and_a_resume_brings_it_back() {
    let (_root, project, runner, _engine) = setup();
    let run = runner.open_folder(&project, None, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    run.park(crate::Park::Pause, &RunOptions::new(), &Recorder::new()).await.expect("parked");
    assert_eq!(run.status().await.expect("status").state, RunState::Parked);
    run.resume(&RunOptions::new(), &Recorder::new()).await.expect("resumed");
    assert_eq!(run.status().await.expect("status").state, RunState::Ready);
}

#[tokio::test]
async fn a_recorded_run_reopens_from_its_folder_and_a_folder_of_nothing_is_refused_before_any_tool() {
    let (root, project, runner, _engine) = setup();
    let name = RunName::new("lab-1").expect("a name");
    runner
        .open_folder(&project, None, Some(name.clone()))
        .expect("opened")
        .up(&RunOptions::new(), &Recorder::new())
        .await
        .expect("up");
    let again = runner.reopen_folder(&name).expect("reopened");
    assert_eq!(again.project().dir, project);
    assert_eq!(again.status().await.expect("status").state, RunState::Ready);

    let empty = root.path().join("empty");
    std::fs::create_dir(&empty).expect("a folder");
    let refused = runner.open_folder(&empty, None, None).err().expect("nothing to run");
    assert_eq!(refused.code(), ErrorCode::ProjectInvalid);
}

#[tokio::test]
async fn a_value_given_reaches_the_tool_and_a_secret_never_shows_in_an_event() {
    let (_root, project, runner, _engine) = setup();
    let run = runner.open_folder(&project, None, None).expect("opened");
    let mut secrets = crate::Secrets::new();
    secrets.insert("TOKEN", crate::SecretValue::new("s3cr3t-value"));
    let opts = RunOptions::new().with_env("ATTACK_IMAGE", "kali").with_secrets(secrets);
    let events = Recorder::new();
    run.up(&opts, &events).await.expect("up");
    let json = serde_json::to_string(&events.events()).expect("json");
    assert!(!json.contains("s3cr3t-value"), "{json}");
}
