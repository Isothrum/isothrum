//! A delivered value never shows where the runner writes or speaks, whatever the run goes through
//! and however a tool prints it: in no file under the runner's home (the run's record, its event
//! log, its state), in no event, and in no error. The `flag-portal` lab delivers through `env`
//! only; the owner-only files a `state_file` or `guest` delivery keeps in the run's state folder
//! are tested with those deliveries (`run/tests/cloud/secrets.rs`). A secret's contract relies on it: a value
//! given to a run reaches that run and nothing else.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Engine, FLAG, answer, fixture, fixture_answering, opts};
use crate::process::fake::{Call, Reply};
use crate::testing::{FLAG_PORTAL, FLAG_PORTAL_ACCESS};
use crate::{AccessPick, Park, Recorder, RunOptions, Running};

/// Every file under `dir` whose bytes hold `value`.
fn files_holding(dir: &Path, value: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .flat_map(|e| {
            let path = e.path();
            if path.is_dir() {
                files_holding(&path, value)
            } else if std::fs::read(&path).is_ok_and(|b| String::from_utf8_lossy(&b).contains(value)) {
                vec![path]
            } else {
                Vec::new()
            }
        })
        .collect()
}

/// The fake engine, its `up` failing with the value it was given in its error lines.
fn up_fails_saying_the_flag(engine: &Mutex<Engine>, call: &Call) -> Reply {
    if call.line().contains(" up --detach ") {
        let flag = call.env("CTF_FLAG_MAIN").unwrap_or_default();
        return Reply::fail(1, &format!("Error response from daemon: can't place {flag} in the secret"));
    }
    answer(engine, call)
}

#[tokio::test]
async fn a_delivered_value_lands_in_no_file_and_no_event_through_a_whole_run() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    run.status().await.unwrap();
    run.check(&opts(), &events).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &events).await.unwrap();
    // An embedder may give the values again at a resume: they are masked there too.
    run.resume(&opts(), &events).await.unwrap();
    run.down(&RunOptions::default(), &events).await.unwrap();
    assert!(f.fake.calls().iter().any(|c| c.env("CTF_FLAG_MAIN") == Some(FLAG)), "the value was delivered");
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains(FLAG) && said.contains("seeding ***"), "{said}");
    assert_eq!(files_holding(f.home(), FLAG), Vec::<PathBuf>::new());
}

#[tokio::test]
async fn a_failed_step_that_prints_the_value_shows_it_in_no_error_event_or_file() {
    let f = fixture_answering(FLAG_PORTAL, up_fails_saying_the_flag);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let err = run.up(&opts(), &events).await.unwrap_err();
    let (shown, debug) = (err.to_string(), format!("{err:?}"));
    assert!(
        !shown.contains(FLAG) && !debug.contains(FLAG) && shown.contains("can't place *** in the secret"),
        "{shown}"
    );
    assert!(!serde_json::to_string(&events.events()).unwrap().contains(FLAG));
    assert_eq!(files_holding(f.home(), FLAG), Vec::<PathBuf>::new());
}

/// The fake engine, every tool it runs also printing the value on its standard error.
fn every_tool_says_the_flag(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let mut reply = answer(engine, call);
    reply.stderr.push(format!("note: {FLAG}"));
    reply
}

/// One operation on a run, its events going to the recorder.
type Operate = for<'a> fn(&'a Running<'a>, &'a Recorder) -> crate::BoxFuture<'a, ()>;

#[tokio::test]
async fn every_operation_masks_the_values_in_its_events() {
    // Each runs on a lab that is up; whether it succeeds doesn't matter, only what its events say.
    let table: [(&str, Operate); 10] = [
        ("up", |r, e| Box::pin(async move { drop(r.up(&opts(), e).await) })),
        ("update", |r, e| Box::pin(async move { drop(r.update(&opts(), e).await) })),
        ("check", |r, e| Box::pin(async move { drop(r.check(&opts(), e).await) })),
        ("provision", |r, e| Box::pin(async move { drop(r.provision(None, &opts(), e).await) })),
        ("attach", |r, e| {
            Box::pin(async move { drop(r.attach(AccessPick::default(), &opts(), e).await) })
        }),
        ("detach", |r, e| Box::pin(async move { drop(r.detach(None, &opts(), e).await) })),
        ("retire", |r, e| Box::pin(async move { drop(r.retire("db", &opts(), e).await) })),
        // Park given no options: the value comes with the run's connection.
        ("park", |r, e| {
            Box::pin(async move { drop(r.park(Park::Shutdown, &RunOptions::new(), e).await) })
        }),
        ("resume", |r, e| Box::pin(async move { drop(r.resume(&opts(), e).await) })),
        ("down", |r, e| Box::pin(async move { drop(r.down(&opts(), e).await) })),
    ];
    // The lab with a set-up to run again, so a provision runs a tool too.
    let mut manifest: serde_json::Value = serde_json::from_str(FLAG_PORTAL_ACCESS).unwrap();
    manifest["lifecycle"]["provision"] = serde_json::json!({
        "all": [{ "id": "provision", "argv": ["docker", "compose", "--project-name", "${run.name}", "exec", "web", "true"], "timeout_s": 60 }],
        "machine": []
    });
    let manifest = manifest.to_string();
    for (op, operate) in table {
        let f = fixture_answering(&manifest, every_tool_says_the_flag);
        let run = f.runner.open(&f.manifest, None).unwrap().with_connection(opts().secrets);
        if op != "up" {
            run.up(&opts(), &Recorder::new()).await.unwrap();
        }
        if op == "resume" {
            run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
        }
        let events = Recorder::new();
        operate(&run, &events).await;
        let said = serde_json::to_string(&events.events()).unwrap();
        assert!(!said.contains(FLAG), "{op}: {said}");
        assert!(said.contains("note: ***"), "{op} ran a tool whose output it showed masked: {said}");
    }
}
