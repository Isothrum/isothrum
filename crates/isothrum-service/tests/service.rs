//! The service over its real endpoint, with a scripted engine: the queue per run, two clients,
//! replay after a reconnection, one service per home, cancel, shutdown, the idle stop, the
//! reaper, and the tools ended when it stops.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use isothrum_client::proto::{Command, OpOptions, Reply, Target};
use isothrum_client::v1::{Event, OperationPhase, is_outcome, phase_of};
use isothrum_client::{Client, ClientError, Followed};
use isothrum_runner::store::OperationId;
use isothrum_runner::{BoxFuture, ErrorCode, EventKind, RunName, Status};
use isothrum_service::{Engine, Journal, Refusal, ServeError, ServeOptions, Service};
use isothrum_test_support::assert_every;

mod common;
use common::{Outcome, PATIENCE, Serving};

/// An engine whose operations log when each begins and ends; one whose `hold` param is set
/// waits, once begun, until the test releases its run. A run is named after its path's file name.
#[derive(Default)]
struct Scripted {
    log: Mutex<Vec<String>>,
    /// Each run's gate, its held operations wait on.
    gates: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
    /// The runs past their lifetime, as the reaper reads them.
    expired: Mutex<Vec<Target>>,
    /// How many times the reaper read them.
    expired_reads: AtomicUsize,
}

impl Scripted {
    fn note(&self, line: String) {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).push(line);
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn gate(&self, run: &str) -> Arc<tokio::sync::Notify> {
        let mut gates = self.gates.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(gates.entry(run.to_owned()).or_default())
    }

    /// Lets `run`'s held operation (the one holding now, or the next) end.
    fn release(&self, run: &str) {
        self.gate(run).notify_one();
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
            let held = options.env.contains_key("hold");
            self.note(format!("begin {run} {op}"));
            let step = EventKind::Log {
                step: "work".into(),
                stream: isothrum_runner::Stream::Stdout,
                line: format!("{op} on {run}"),
            };
            journal.keep(isothrum_runner::Event::new(1, Some(run.to_string()), Some(op), step));
            if held {
                self.gate(run.as_str()).notified().await;
            }
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
        self.expired_reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.expired.lock().unwrap_or_else(PoisonError::into_inner).clone())
    }

    fn status<'a>(&'a self, _run: &'a RunName, _target: Option<&'a Target>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async { Err(Refusal::new(ErrorCode::NotRunning, "scripted")) })
    }
}

/// A service on a fresh home, its engine, and its task.
struct Running {
    home: tempfile::TempDir,
    engine: Arc<Scripted>,
    task: Serving,
}

async fn start(idle: Option<Duration>) -> Outcome<Running> {
    start_in(tempfile::tempdir()?, idle).await
}

async fn start_in(home: tempfile::TempDir, idle: Option<Duration>) -> Outcome<Running> {
    start_with(home, Scripted::default(), |options| options.idle = idle).await
}

async fn start_with(home: tempfile::TempDir, engine: Scripted, set: impl FnOnce(&mut ServeOptions)) -> Outcome<Running> {
    let engine = Arc::new(engine);
    let shared: Arc<dyn Engine> = Arc::<Scripted>::clone(&engine);
    let mut options = ServeOptions::new(shared, common::never());
    set(&mut options);
    let (_, task) = common::serve_with(home.path(), options).await?;
    Ok(Running { home, engine, task })
}

/// What an operation's options say: held until released, or not.
fn options(held: bool) -> OpOptions {
    OpOptions {
        env: if held {
            [("hold".to_owned(), "1".to_owned())].into()
        } else {
            std::collections::BTreeMap::default()
        },
        ..OpOptions::default()
    }
}

/// An `up` of `run`, held until the test releases it or not.
fn up(run: &str, held: bool) -> Command {
    Command::Up {
        target: Target::new(PathBuf::from("/runs").join(run), None),
        options: options(held),
        lifetime_s: None,
    }
}

fn down(run: &str) -> Command {
    Command::Down {
        target: Target::new(PathBuf::from("/runs").join(run), None),
        options: options(false),
    }
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

/// Whether `followed` ends cancelled (within [`PATIENCE`]).
async fn ends_cancelled(followed: &mut Followed) -> bool {
    let outcome = tokio::time::timeout(PATIENCE, followed.outcome()).await;
    matches!(
        outcome,
        Ok(Err(ClientError::Operation(ref report))) if report.code == ErrorCode::Cancelled
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_clients_operations_on_one_run_run_in_order_and_other_runs_alongside() {
    let s = start(None).await.unwrap();
    let (a, b) = (
        Client::connect_running(s.home.path()).await.unwrap(),
        Client::connect_running(s.home.path()).await.unwrap(),
    );
    let mut first = a.follow(up("web", true)).await.unwrap();
    let mut second = b.follow(down("web")).await.unwrap();
    let mut other = b.follow(up("db", false)).await.unwrap();
    assert_eq!((first.accepted.position, second.accepted.position, other.accepted.position), (0, 1, 0));
    // Another run's operation runs to its end while web's first holds.
    assert!(events_of(&mut other).await.unwrap().iter().any(is_outcome));
    let held = s.engine.log();
    let said = |line: &str| held.iter().any(|l| l == line);
    assert!(said("end db up") && !said("end web up") && !said("begin web down"), "{held:?}");
    s.engine.release("web");
    let (first_events, second_events) = tokio::join!(events_of(&mut first), events_of(&mut second));
    let (first_events, second_events) = (first_events.unwrap(), second_events.unwrap());
    let log = s.engine.log();
    let at = |line: &str| log.iter().position(|l| l == line).unwrap_or_else(|| panic!("{line} in {log:?}"));
    assert!(at("end web up") < at("begin web down"), "one run's operations one after the other: {log:?}");
    assert!(at("end db up") < at("end web up"), "another run's alongside: {log:?}");
    let phases: Vec<_> = second_events.iter().filter_map(phase_of).collect();
    assert_eq!(phases, [OperationPhase::Queued, OperationPhase::Started, OperationPhase::Ended]);
    assert!(first_events.iter().any(is_outcome) && second_events.iter().any(is_outcome));
    assert_every!(second_events.iter(), |e: &&Event| e.operation == Some(second.accepted.operation));
    let ops = a.operations(&RunName::new("web").unwrap(), 10).await.unwrap();
    assert_eq!(ops.len(), 2);
    assert_every!(ops.iter(), |o: &&isothrum_client::v1::OperationRow| o.ended.is_some());
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reconnecting_subscriber_resumes_after_its_last_cursor_and_misses_nothing() {
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut all = client.subscribe(None, None).await.unwrap();
    client.follow(up("web", false)).await.unwrap().outcome().await.unwrap();
    let mut seen = Vec::new();
    while seen.len() < 5 {
        seen.push(all.next().await.unwrap().unwrap());
    }
    let last = seen.last().unwrap().0;
    drop(all);
    // While nobody listens: another operation.
    client.follow(down("web")).await.unwrap().outcome().await.unwrap();
    let mut resumed = client.subscribe(None, Some(last)).await.unwrap();
    let mut missed = Vec::new();
    while missed.len() < 5 {
        missed.push(resumed.next().await.unwrap().unwrap());
    }
    assert_every!(missed.iter(), |(c, _): &&(isothrum_client::v1::EventCursor, Event)| *c > last);
    assert_every!(missed.windows(2), |w: &&[(isothrum_client::v1::EventCursor, Event)]| w[0].0 < w[1].0);
    assert_eq!(phase_of(&missed.first().unwrap().1), Some(OperationPhase::Queued), "from the first missed one");
    assert_every!(missed.iter(), |(_, e): &&(isothrum_client::v1::EventCursor, Event)| e.op
        == Some(isothrum_runner::Op::Down));
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
    let mut long = client.follow(up("web", true)).await.unwrap();
    let mut queued = client.follow(down("web")).await.unwrap();
    client.cancel(queued.accepted.operation).await.unwrap();
    let dropped = events_of(&mut queued).await.unwrap();
    assert!(!dropped.iter().any(|e| phase_of(e) == Some(OperationPhase::Started)), "{dropped:?}");
    assert!(
        dropped
            .iter()
            .any(|e| matches!(&e.kind, EventKind::Error(report) if report.code == ErrorCode::Cancelled))
    );
    // Wait for the long one to begin, then cut it short.
    begun(&mut long).await.unwrap();
    client.cancel(long.accepted.operation).await.unwrap();
    assert!(ends_cancelled(&mut long).await);
    assert!(!s.engine.log().contains(&"end web up".to_owned()));
    let refused = client.cancel(long.accepted.operation).await;
    assert!(matches!(refused, Err(ClientError::Refused(_))), "an ended operation: {refused:?}");
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_run_is_answered_at_once() {
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let refused = client.submit(up("missing", false)).await;
    assert!(
        matches!(&refused, Err(ClientError::Refused(report)) if report.code == ErrorCode::Unsupported),
        "{refused:?}"
    );
    s.task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_cancels_the_running_operation_and_the_service_stops() {
    let s = start(None).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    let mut long = client.follow(up("web", true)).await.unwrap();
    begun(&mut long).await.unwrap();
    client.shutdown().await.unwrap();
    assert!(ends_cancelled(&mut long).await);
    common::ended(s.task).await.unwrap();
    assert!(!s.home.path().join("service.json").exists(), "it says it is gone");
    let gone = Client::connect_running(s.home.path()).await.err();
    assert!(matches!(gone, Some(ClientError::Unavailable(_))), "{gone:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_idle_service_stops_but_not_while_a_client_is_connected() {
    let s = start(Some(Duration::from_millis(300))).await.unwrap();
    let client = Client::connect_running(s.home.path()).await.unwrap();
    // That it doesn't stop takes time to see: more than twice its idle time.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!s.task.is_finished(), "a client is connected");
    drop(client);
    common::ended(s.task).await.unwrap();
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
    let mut long = client.follow(up("web", true)).await.unwrap();
    begun(&mut long).await.unwrap();
    let refused = client.request(Command::ShutdownIfIdle).await.unwrap();
    assert!(
        matches!(&refused, Reply::Busy { operations, .. } if operations == &[long.accepted.operation]),
        "{refused:?}"
    );
    s.engine.release("web");
    long.outcome().await.unwrap();
    assert!(s.engine.log().contains(&"end web up".to_owned()), "the operation ran to its end");
    // The queue says it is idle once the operation's end is journaled, moments after its outcome.
    let deadline = Instant::now() + PATIENCE;
    loop {
        match client.request(Command::ShutdownIfIdle).await.unwrap() {
            Reply::Ok { .. } => break,
            Reply::Busy { .. } if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(20)).await,
            other => panic!("{other:?}"),
        }
    }
    common::ended(s.task).await.unwrap();
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
    let deadline = Instant::now() + PATIENCE;
    while !s.engine.log().contains(&"end old down".to_owned()) {
        assert!(Instant::now() < deadline, "the expired run was never brought down");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Five more looks: the run is still listed (the scripted engine keeps it), never queued again.
    let looked = s.engine.expired_reads.load(Ordering::SeqCst);
    while s.engine.expired_reads.load(Ordering::SeqCst) < looked + 5 {
        assert!(Instant::now() < deadline, "the reaper stopped looking");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
    common::ended(s.task).await.unwrap();
    assert_eq!(s.engine.expired_reads.load(Ordering::SeqCst), 0, "no reaper ran");
    assert_eq!(s.engine.log(), Vec::<String>::new());
}

/// An engine whose operation starts one real tool through `tools`, which writes its pid to
/// `pidfile`, ignores the request to stop (SIGINT, SIGTERM), and runs on.
#[cfg(unix)]
struct Stubborn {
    tools: isothrum_runner::SystemProcesses,
    pidfile: PathBuf,
}

#[cfg(unix)]
impl Engine for Stubborn {
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal> {
        RunName::new(target.path.file_name().and_then(|n| n.to_str()).unwrap_or_default()).map_err(|e| Refusal::new(ErrorCode::RunName, e.to_string()))
    }

    fn execute<'a>(&'a self, _run: &'a RunName, _command: &'a Command, _operation: OperationId, _journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            use isothrum_runner::ProcessRunner as _;
            let script = r#"trap '' INT TERM; echo $$ > "$0"; exec sleep 300"#;
            let spec = isothrum_runner::ProcessSpec::new("sh", ["-c", script, self.pidfile.to_str().unwrap_or_default()]);
            self.tools.run(&spec, &mut |_, _| {}).await.is_ok()
        })
    }

    fn status<'a>(&'a self, _run: &'a RunName, _target: Option<&'a Target>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async { Err(Refusal::new(ErrorCode::NotRunning, "stubborn")) })
    }
}

/// `ServeOptions::tools` at the stop: the running operation is cancelled, and its tool, which
/// ignores the request to stop, is ended at once rather than after the tools' 30 s grace, before
/// `serve` returns.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stopping_service_ends_the_tools_its_options_give_before_it_returns() {
    use isothrum_test_support::{PidGuard, alive, read_pid, wait_until};
    let (home, dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let pidfile = dir.path().join("tool.pid");
    let tools = isothrum_runner::SystemProcesses::default();
    let engine = Arc::new(Stubborn {
        tools: tools.clone(),
        pidfile: pidfile.clone(),
    });
    let mut options = ServeOptions::new(engine, common::never());
    options.tools = Some(tools);
    options.reap_every = None;
    let (client, task) = common::serve_with(home.path(), options).await.unwrap();
    let mut running = client.follow(up("web", false)).await.unwrap();
    assert!(wait_until(PATIENCE, || read_pid(&pidfile).is_some()), "the tool started");
    let tool = PidGuard::from_file(&pidfile).unwrap();
    assert!(alive(tool.id()));

    let asked = Instant::now();
    client.shutdown().await.unwrap();
    assert!(ends_cancelled(&mut running).await);
    common::ended(task).await.unwrap();
    assert!(!alive(tool.id()), "the tool {} outlived the service", tool.id());
    assert!(
        asked.elapsed() < Duration::from_secs(30),
        "ended at once, not after the grace: {:?}",
        asked.elapsed()
    );
}
