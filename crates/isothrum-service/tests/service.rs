//! The service over its real endpoint, with a scripted engine: the queue per run, two clients,
//! replay after a reconnection, one service per home, cancel, shutdown and the idle stop.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use isothrum_client::proto::{Command, OpOptions, Reply, Target};
use isothrum_client::v1::{Event, OperationPhase, is_outcome, phase_of};
use isothrum_client::{Client, ClientError, Followed};
use isothrum_runner::store::OperationId;
use isothrum_runner::{BoxFuture, ErrorCode, EventKind, RunName, Status};
use isothrum_service::{Engine, Journal, Refusal, ServeError, ServeOptions, Service, StopSignal};

/// An engine whose operations sleep for their `sleep_ms` param, logging when each begins and
/// ends; a run is named after its path's file name.
#[derive(Default)]
struct Scripted {
    log: Mutex<Vec<String>>,
    /// The runs past their lifetime, as the reaper reads them.
    expired: Mutex<Vec<Target>>,
    /// How many times the reaper read them.
    expired_reads: std::sync::atomic::AtomicUsize,
}

impl Scripted {
    fn note(&self, line: String) {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(line);
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
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

    fn execute<'a>(&'a self, run: &'a RunName, command: &'a Command, _operation: OperationId, journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let (Some((_, options)), Some(op)) = (command.target(), command.op()) else {
                return false;
            };
            let sleep: u64 = options.env.get("sleep_ms").and_then(|s| s.parse().ok()).unwrap_or(0);
            self.note(format!("begin {run} {op}"));
            let step = EventKind::Log {
                step: "work".into(),
                stream: isothrum_runner::Stream::Stdout,
                line: format!("{op} on {run}"),
            };
            journal.keep(isothrum_runner::Event::new(1, Some(run.to_string()), Some(op), step));
            tokio::time::sleep(Duration::from_millis(sleep)).await;
            self.note(format!("end {run} {op}"));
            let result = EventKind::Result {
                ok: true,
                data: isothrum_runner::ResultData::Moved {
                    state: isothrum_runner::RunState::Ready,
                },
            };
            journal.keep(isothrum_runner::Event::new(2, Some(run.to_string()), Some(op), result));
            true
        })
    }

    fn expired(&self) -> Result<Vec<Target>, Refusal> {
        self.expired_reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.expired.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    fn status<'a>(&'a self, _run: &'a RunName, _target: Option<&'a Target>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async { Err(Refusal::new(ErrorCode::NotRunning, "scripted")) })
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

/// What a helper's failure fails the test with.
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

async fn start(idle: Option<Duration>) -> Outcome<Running> {
    start_in(tempfile::tempdir()?, idle).await
}

async fn start_in(home: tempfile::TempDir, idle: Option<Duration>) -> Outcome<Running> {
    start_with(home, Scripted::default(), |options| options.idle = idle).await
}

async fn start_with(home: tempfile::TempDir, engine: Scripted, set: impl FnOnce(&mut ServeOptions)) -> Outcome<Running> {
    let engine = Arc::new(engine);
    let service = Service::hold(home.path())?;
    let shared: Arc<dyn Engine> = Arc::<Scripted>::clone(&engine);
    let mut options = ServeOptions::new(shared, never());
    set(&mut options);
    let task = tokio::spawn(service.serve(options));
    for _ in 0..200 {
        if Client::connect_running(home.path()).await.is_ok() {
            return Ok(Running { home, engine, task });
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("the service didn't answer".into())
}

fn up(run: &str, sleep_ms: u64) -> Command {
    Command::Up {
        target: Target::new(PathBuf::from("/runs").join(run), None),
        options: OpOptions {
            env: [("sleep_ms".to_owned(), sleep_ms.to_string())].into(),
            ..OpOptions::default()
        },
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
async fn events_of(followed: &mut Followed) -> Result<Vec<Event>, ClientError> {
    let mut all = Vec::new();
    while let Some(event) = followed.next().await {
        all.push(event?);
    }
    Ok(all)
}

/// Waits for `followed` to begin.
async fn begun(followed: &mut Followed) -> Result<(), ClientError> {
    while let Some(event) = followed.next().await {
        if phase_of(&event?) == Some(OperationPhase::Started) {
            return Ok(());
        }
    }
    Err(ClientError::Protocol("it ended before it began".to_owned()))
}

/// Whether `followed` ends cancelled within 5 seconds.
async fn ends_cancelled(followed: &mut Followed) -> bool {
    let outcome = tokio::time::timeout(Duration::from_secs(5), followed.outcome()).await;
    matches!(
        outcome,
        Ok(Err(ClientError::Operation {
            code: ErrorCode::Cancelled,
            ..
        }))
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_operations_on_one_run_run_in_order_and_other_runs_alongside() {
    let s = start(None).await.unwrap();
    let (a, b) = (
        Client::connect_running(s.home.path()).await.unwrap(),
        Client::connect_running(s.home.path()).await.unwrap(),
    );
    let mut first = a.follow(up("web", 300)).await.unwrap();
    let mut second = b.follow(down("web", 0)).await.unwrap();
    let mut other = b.follow(up("db", 0)).await.unwrap();
    assert_eq!((first.accepted.position, second.accepted.position, other.accepted.position), (0, 1, 0));
    let (first_events, second_events, _) = tokio::join!(events_of(&mut first), events_of(&mut second), events_of(&mut other));
    let (first_events, second_events) = (first_events.unwrap(), second_events.unwrap());
    let log = s.engine.log();
    let at = |line: &str| log.iter().position(|l| l == line).unwrap_or_else(|| panic!("{line} in {log:?}"));
    assert!(at("end web up") < at("begin web down"), "one run's operations one after the other: {log:?}");
    assert!(at("end db up") < at("end web up"), "another run's alongside: {log:?}");
    let phases: Vec<_> = second_events.iter().filter_map(phase_of).collect();
    assert_eq!(phases, [OperationPhase::Queued, OperationPhase::Started, OperationPhase::Ended]);
    assert!(first_events.iter().any(is_outcome) && second_events.iter().any(is_outcome));
    assert!(second_events.iter().all(|e| e.operation == Some(second.accepted.operation)));
    let ops = a.operations(&RunName::new("web").unwrap(), 10).await.unwrap();
    assert_eq!(ops.len(), 2);
    assert!(ops.iter().all(|o| o.ended.is_some()), "{ops:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnecting_subscriber_resumes_after_its_last_cursor_and_misses_nothing() {
    let s = start(None).await.unwrap();
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
    assert_eq!(phase_of(&missed.first().unwrap().1), Some(OperationPhase::Queued), "from the first missed one");
    assert!(missed.iter().all(|(_, e)| e.op == Some(isothrum_runner::Op::Down)));
    let only_db = client.subscribe(Some(RunName::new("db").unwrap()), None).await.unwrap();
    drop(only_db);
    s.task.abort();
}

#[tokio::test]
async fn one_service_per_home() {
    let s = start(None).await.unwrap();
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
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", 60_000)).await.unwrap();
    let mut queued = client.follow(down("web", 0)).await.unwrap();
    client.cancel(queued.accepted.operation).await.unwrap();
    let dropped = events_of(&mut queued).await.unwrap();
    assert!(!dropped.iter().any(|e| phase_of(e) == Some(OperationPhase::Started)), "{dropped:?}");
    assert!(dropped.iter().any(|e| matches!(
        e.kind,
        EventKind::Error {
            code: ErrorCode::Cancelled,
            ..
        }
    )));
    // Wait for the long one to begin, then cut it short.
    begun(&mut long).await.unwrap();
    client.cancel(long.accepted.operation).await.unwrap();
    assert!(ends_cancelled(&mut long).await);
    assert!(!s.engine.log().contains(&"end web up".to_owned()));
    let refused = client.cancel(long.accepted.operation).await;
    assert!(matches!(refused, Err(ClientError::Refused { .. })), "an ended operation: {refused:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_run_is_answered_at_once() {
    let s = start(None).await.unwrap();
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
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", 60_000)).await.unwrap();
    begun(&mut long).await.unwrap();
    client.shutdown().await.unwrap();
    assert!(ends_cancelled(&mut long).await);
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
    assert!(!s.home.path().join("service.json").exists(), "it says it is gone");
    assert!(Client::connect_running(s.home.path()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_service_stops_but_not_while_a_client_is_connected() {
    let s = start(Some(Duration::from_millis(300))).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!s.task.is_finished(), "a client is connected");
    drop(client);
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_services_queued_operations_are_cancelled_when_the_next_one_starts() {
    let home = tempfile::tempdir().unwrap();
    let run = RunName::new("web").unwrap();
    let left = {
        let store = isothrum_runner::store::Store::open(home.path()).unwrap();
        let mut dead = isothrum_runner::store::Owner::this_process();
        // A pid no process has: the owner is gone.
        dead.pid = u32::MAX - 1;
        store.write(|tx| tx.queue_operation(&run, isothrum_runner::Op::Up, &dead)).unwrap().id
    };
    let s = start_in(home, None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let ops = client.operations(&run, 10).await.unwrap();
    let row = ops.iter().find(|o| o.id == left).unwrap();
    assert_eq!(row.ended.as_ref().map(|e| e.outcome), Some(isothrum_client::v1::Outcome::Cancelled), "{row:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_if_idle_is_refused_while_an_operation_runs_and_stops_the_service_once_idle() {
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", 500)).await.unwrap();
    begun(&mut long).await.unwrap();
    let refused = client.request(Command::ShutdownIfIdle).await.unwrap();
    assert!(
        matches!(&refused, Reply::Busy { operations, .. } if operations == &[long.accepted.operation]),
        "{refused:?}"
    );
    long.outcome().await.unwrap();
    assert!(s.engine.log().contains(&"end web up".to_owned()), "the operation ran to its end");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match client.request(Command::ShutdownIfIdle).await.unwrap() {
            Reply::Ok { .. } => break,
            Reply::Busy { .. } if std::time::Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(20)).await,
            other => panic!("{other:?}"),
        }
    }
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
}

/// The reaper runs in the service: a run past its lifetime gets its `down` queued once, with no
/// client asking, and isn't queued again while the first is recent.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_past_its_lifetime_is_brought_down_by_the_service_itself() {
    let engine = Scripted::default();
    *engine.expired.lock().unwrap() = vec![Target::new(PathBuf::from("/runs/old"), RunName::new("old").ok())];
    let s = start_with(tempfile::tempdir().unwrap(), engine, |o| o.reap_every = Some(Duration::from_millis(20)))
        .await
        .unwrap();
    for _ in 0..500 {
        if s.engine.log().contains(&"end old down".to_owned()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Several more looks: the run is still listed (the scripted engine keeps it), never queued again.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(s.engine.log(), ["begin old down", "end old down"]);
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_service_told_not_to_reap_leaves_an_expired_run() {
    let engine = Scripted::default();
    *engine.expired.lock().unwrap() = vec![Target::new(PathBuf::from("/runs/old"), RunName::new("old").ok())];
    let s = start_with(tempfile::tempdir().unwrap(), engine, |o| o.reap_every = None).await.unwrap();
    // The service's whole life, from its start to its end: it never looks for expired runs.
    let client = Client::connect_running(s.home.path()).await.unwrap();
    assert!(matches!(client.request(Command::ShutdownIfIdle).await.unwrap(), Reply::Ok { .. }));
    tokio::time::timeout(Duration::from_secs(5), s.task).await.unwrap().unwrap().unwrap();
    assert_eq!(s.engine.expired_reads.load(std::sync::atomic::Ordering::SeqCst), 0, "no reaper ran");
    assert_eq!(s.engine.log(), Vec::<String>::new());
}
