//! The service over its real endpoint, with a scripted engine: the queue per run, two clients,
//! replay after a reconnection, one service per home, cancel, shutdown and the idle stop.

#![expect(clippy::unwrap_used, clippy::panic, reason = "the scripted engine and the helpers fail the test they serve")]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_client::v1::{Event, OperationPhase, RunRow};
use isothrum_client::{Client, ClientError};
use isothrum_runner::{BoxFuture, ErrorCode, EventKind, Events, RunName, Status};
use isothrum_service::{Engine, Refusal, ServeError, ServeOptions, Service, StopSignal};

/// An engine whose operations sleep for their `sleep_ms` param, logging when each begins and
/// ends; a run is named after its path's file name.
#[derive(Default)]
struct Scripted {
    log: Mutex<Vec<String>>,
}

impl Scripted {
    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

impl Engine for Scripted {
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal> {
        let name = target.path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name == "missing" {
            return Err(Refusal::new(ErrorCode::Unsupported, "nothing there"));
        }
        RunName::new(name).map_err(|e| Refusal::new(ErrorCode::RunName, e.to_string()))
    }

    fn execute<'a>(&'a self, run: &'a RunName, command: &'a Command, events: &'a dyn Events) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let (_, options) = command.target().unwrap();
            let op = command.op().unwrap();
            let sleep: u64 = options.params.get("sleep_ms").map_or(0, |s| s.parse().unwrap());
            self.log.lock().unwrap().push(format!("begin {run} {op}"));
            let step = EventKind::Log {
                step: "work".into(),
                stream: isothrum_runner::Stream::Stdout,
                line: format!("{op} on {run}"),
            };
            events.emit(&isothrum_runner::Event::new(1, Some(run.to_string()), Some(op), step));
            tokio::time::sleep(Duration::from_millis(sleep)).await;
            self.log.lock().unwrap().push(format!("end {run} {op}"));
            let result = EventKind::Result {
                ok: true,
                data: serde_json::json!({ "message": "done" }),
            };
            events.emit(&isothrum_runner::Event::new(2, Some(run.to_string()), Some(op), result));
            true
        })
    }

    fn status<'a>(&'a self, _run: &'a RunName, _path: Option<&'a Path>) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async { Err(Refusal::new(ErrorCode::NotRunning, "scripted")) })
    }

    fn list(&self) -> Result<Vec<RunRow>, Refusal> {
        Ok(Vec::new())
    }
}

/// A stop signal that never fires.
fn never() -> StopSignal {
    Box::new(|| Box::pin(std::future::pending()))
}

/// A service on a fresh home, its engine, and its task.
struct Running {
    home: tempfile::TempDir,
    engine: Arc<Scripted>,
    task: tokio::task::JoinHandle<Result<(), ServeError>>,
}

async fn start(idle: Option<Duration>) -> Running {
    let home = tempfile::tempdir().unwrap();
    let engine = Arc::new(Scripted::default());
    let service = Service::hold(home.path()).unwrap();
    let shared: Arc<dyn Engine> = Arc::<Scripted>::clone(&engine);
    let mut options = ServeOptions::new(shared, never());
    options.idle = idle;
    let task = tokio::spawn(service.serve(options));
    for _ in 0..200 {
        if Client::connect_running(home.path()).await.is_ok() {
            return Running { home, engine, task };
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the service didn't answer");
}

fn up(run: &str, sleep_ms: u64) -> Command {
    Command::Up {
        target: Target::new(PathBuf::from("/runs").join(run), None),
        options: OpOptions {
            params: [("sleep_ms".to_owned(), sleep_ms.to_string())].into(),
            ..OpOptions::default()
        },
        fresh: false,
        lifetime_s: None,
    }
}

fn down(run: &str, sleep_ms: u64) -> Command {
    let Command::Up { target, options, .. } = up(run, sleep_ms) else {
        unreachable!()
    };
    Command::Down { target, options }
}

/// Every event of a followed operation.
async fn events_of(followed: &mut isothrum_client::Followed) -> Vec<Event> {
    let mut all = Vec::new();
    while let Some(event) = followed.next().await {
        all.push(event.unwrap());
    }
    all
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_operations_on_one_run_run_in_order_and_other_runs_alongside() {
    let s = start(None).await;
    let (a, b) = (
        Client::connect_running(s.home.path()).await.unwrap(),
        Client::connect_running(s.home.path()).await.unwrap(),
    );
    let mut first = a.follow(up("web", 300)).await.unwrap();
    let mut second = b.follow(down("web", 0)).await.unwrap();
    let mut other = b.follow(up("db", 0)).await.unwrap();
    assert_eq!((first.accepted.position, second.accepted.position, other.accepted.position), (0, 1, 0));
    let (first_events, second_events, _) = tokio::join!(events_of(&mut first), events_of(&mut second), events_of(&mut other));
    let log = s.engine.log();
    let at = |line: &str| log.iter().position(|l| l == line).unwrap_or_else(|| panic!("{line} in {log:?}"));
    assert!(at("end web up") < at("begin web down"), "one run's operations one after the other: {log:?}");
    assert!(at("end db up") < at("end web up"), "another run's alongside: {log:?}");
    let phases: Vec<_> = second_events.iter().filter_map(Event::phase_of).collect();
    assert_eq!(phases, [OperationPhase::Queued, OperationPhase::Started, OperationPhase::Ended]);
    assert!(first_events.iter().any(Event::is_outcome) && second_events.iter().any(Event::is_outcome));
    assert!(second_events.iter().all(|e| e.operation == Some(second.accepted.operation)));
    let ops = a.operations(&RunName::new("web").unwrap(), 10).await.unwrap();
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|o| o.ended.is_some()), "{ops:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnecting_subscriber_resumes_after_its_last_cursor_and_misses_nothing() {
    let s = start(None).await;
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut all = client.subscribe(None, None).await.unwrap();
    client.follow(up("web", 0)).await.unwrap().outcome().await.unwrap();
    let mut seen = Vec::new();
    while seen.len() < 5 {
        seen.push(all.next().await.unwrap().unwrap());
    }
    let last = seen.last().unwrap().0;
    drop(all);
    // While nobody listens: another operation.
    client.follow(down("web", 0)).await.unwrap().outcome().await.unwrap();
    let mut resumed = client.subscribe(None, Some(last)).await.unwrap();
    let mut missed = Vec::new();
    while missed.len() < 5 {
        missed.push(resumed.next().await.unwrap().unwrap());
    }
    assert!(missed.iter().all(|(c, _)| *c > last), "only what came after");
    assert!(missed.windows(2).all(|w| w[0].0 < w[1].0), "in order");
    assert_eq!(missed.first().unwrap().1.phase_of(), Some(OperationPhase::Queued), "from the first missed one");
    assert!(missed.iter().all(|(_, e)| e.op == Some(isothrum_runner::Op::Down)));
    let only_db = client.subscribe(Some(RunName::new("db").unwrap()), None).await.unwrap();
    drop(only_db);
    s.task.abort();
}

#[tokio::test]
async fn one_service_per_home() {
    let s = start(None).await;
    let held = Service::hold(s.home.path()).unwrap_err();
    assert!(
        matches!(&held, ServeError::Held { holder: Some(h), .. } if h.pid == std::process::id()),
        "{held:?}"
    );
    assert!(held.to_string().contains("already holds"), "{held}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_operation_ends_cancelled_queued_or_running() {
    let s = start(None).await;
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", 60_000)).await.unwrap();
    let mut queued = client.follow(down("web", 0)).await.unwrap();
    client.cancel(queued.accepted.operation).await.unwrap();
    let dropped = events_of(&mut queued).await;
    assert!(!dropped.iter().any(|e| e.phase_of() == Some(OperationPhase::Started)), "{dropped:?}");
    assert!(dropped.iter().any(|e| matches!(
        e.to_runner().map(|r| r.kind),
        Some(EventKind::Error {
            code: ErrorCode::Cancelled,
            ..
        })
    )));
    // Wait for the long one to begin, then cut it short.
    while long.next().await.unwrap().unwrap().phase_of() != Some(OperationPhase::Started) {}
    client.cancel(long.accepted.operation).await.unwrap();
    let cut = tokio::time::timeout(Duration::from_secs(5), long.outcome()).await.unwrap();
    assert!(
        matches!(
            cut,
            Err(ClientError::Operation {
                code: ErrorCode::Cancelled,
                ..
            })
        ),
        "{cut:?}"
    );
    assert!(!s.engine.log().contains(&"end web up".to_owned()));
    let refused = client.cancel(long.accepted.operation).await;
    assert!(matches!(refused, Err(ClientError::Refused { .. })), "an ended operation: {refused:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_run_is_answered_at_once() {
    let s = start(None).await;
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let refused = client.submit(up("missing", 0)).await;
    assert!(
        matches!(
            refused,
            Err(ClientError::Refused {
                code: ErrorCode::Unsupported,
                ..
            })
        ),
        "{refused:?}"
    );
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_the_running_operation_and_the_service_stops() {
    let s = start(None).await;
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", 60_000)).await.unwrap();
    while long.next().await.unwrap().unwrap().phase_of() != Some(OperationPhase::Started) {}
    client.shutdown().await.unwrap();
    let cut = tokio::time::timeout(Duration::from_secs(5), long.outcome()).await.unwrap();
    assert!(
        matches!(
            cut,
            Err(ClientError::Operation {
                code: ErrorCode::Cancelled,
                ..
            })
        ),
        "{cut:?}"
    );
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
    assert!(!s.home.path().join("service.json").exists(), "it says it is gone");
    assert!(Client::connect_running(s.home.path()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_service_stops_but_not_while_a_client_is_connected() {
    let s = start(Some(Duration::from_millis(300))).await;
    let client = Client::connect_running(s.home.path()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!s.task.is_finished(), "a client is connected");
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
}
