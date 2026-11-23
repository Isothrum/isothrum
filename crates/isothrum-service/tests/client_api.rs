//! The client's public API as a launcher calls it, against a real service on a fresh home: the
//! three ways in, an operation followed to its end and waited for, the subscriptions, and each
//! failure with its code and typed detail. The service's engine is the test's own, so what each
//! call gets back is known; the service's own side is `round_trip.rs` and `service.rs`. It lives
//! here, not in the client's `tests/`: the service depends on the client, and the workspace keeps
//! its dependencies one way.

use std::path::Path;
use std::sync::Arc;

use isothrum_client::proto::{Command, OpOptions, PROTOCOL, Target};
use isothrum_client::v1::{Event, EventKind, OperationPhase, phase_of};
use isothrum_client::{Client, ClientError, VERSION};
use isothrum_runner::store::OperationId;
use isothrum_runner::{BoxFuture, Detail, ErrorCode, ResultData, RunName, RunState, Status};
use isothrum_service::{Engine, Journal, Refusal, ServeOptions};

/// The failure every run named `gone` has: its machine `web` removed outside it.
fn web_gone() -> isothrum_runner::Error {
    isothrum_runner::Error::PartlyGone {
        run: "gone".to_owned(),
        machines: vec!["web".to_owned()],
    }
}

/// An engine for runs named after their folder: `refused` is refused before anything is queued;
/// an operation on `gone` fails at its step `resume`, one on `held` waits for [`HELD`], any other
/// ends well after one line of log. Only `stack` has a status.
struct Stack;

/// What an operation on run `held` waits for before it goes on.
static HELD: tokio::sync::Notify = tokio::sync::Notify::const_new();

impl Engine for Stack {
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal> {
        match target.path.file_name().and_then(|n| n.to_str()).unwrap_or_default() {
            "refused" => Err(Refusal::from(web_gone())),
            name => RunName::new(name).map_err(|e| Refusal::new(ErrorCode::RunName, e.to_string())),
        }
    }

    fn execute<'a>(&'a self, run: &'a RunName, command: &'a Command, _operation: OperationId, journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let (run, op) = (Some(run.to_string()), command.op());
            if run.as_deref() == Some("gone") {
                let e = web_gone();
                let error = EventKind::Error(e.report().with_step("resume"));
                journal.keep(Event::new(1, run, op, error));
                return false;
            }
            if run.as_deref() == Some("held") {
                HELD.notified().await;
            }
            let line = EventKind::Log {
                step: "work".to_owned(),
                stream: isothrum_runner::Stream::Stdout,
                line: "working".to_owned(),
            };
            journal.keep(Event::new(1, run.clone(), op, line));
            let moved = EventKind::Result {
                ok: true,
                data: ResultData::Moved { state: RunState::Ready },
            };
            journal.keep(Event::new(2, run, op, moved));
            true
        })
    }

    fn status<'a>(&'a self, run: &'a RunName, _target: Option<&'a Target>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async move {
            if run.as_str() != "stack" {
                return Err(Refusal::new(ErrorCode::NotRunning, format!("`{run}` isn't running")));
            }
            let ready = serde_json::json!({ "state": "ready", "provider": null, "machines": [], "outputs": [] });
            serde_json::from_value(ready).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
        })
    }
}

mod common;
use common::{Outcome, Serving};

/// A service on `home` over [`Stack`], never stopped but by a request, and a client of it.
async fn serve(home: &Path) -> Outcome<(Client, Serving)> {
    common::serve_with(home, ServeOptions::new(Arc::new(Stack), common::never())).await
}

/// A service on a fresh home, its task, and a client of it.
async fn started() -> Outcome<(tempfile::TempDir, Serving, Client)> {
    let home = tempfile::tempdir()?;
    let (client, serving) = serve(home.path()).await?;
    Ok((home, serving, client))
}

/// An `up` of the run in the folder named `run`.
fn up(run: &str) -> Command {
    Command::Up {
        target: Target::new(Path::new("/stacks").join(run), None),
        options: OpOptions::default(),
        lifetime_s: None,
    }
}

/// What `event` is, in a word: its kind, an operation event by its phase.
fn word(event: &Event) -> String {
    match (&event.kind, phase_of(event)) {
        (_, Some(phase)) => format!("{phase:?}").to_lowercase(),
        (EventKind::Log { line, .. }, None) => format!("log {line}"),
        (EventKind::Result { ok, .. }, None) => format!("result {ok}"),
        (EventKind::Error(report), None) => format!("error {:?}", report.code),
        (other, None) => format!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_finds_no_service_then_the_one_running_by_each_way_in() {
    let home = tempfile::tempdir().expect("a home");
    let none = Client::connect_running(home.path()).await.expect_err("nothing runs yet");
    assert!(matches!(&none, ClientError::Unavailable(why) if why.contains("no service runs for")), "{none}");
    assert_eq!(none.code(), ErrorCode::Io);

    let (running, serving) = serve(home.path()).await.expect("a service answering");
    let info = running.service().clone();
    assert_eq!((info.protocol, info.version.as_str()), (PROTOCOL, VERSION));
    // The service runs in this test, so its build is the test's own: the binary given, as an
    // `isothrum` beside the test (as a test machine places it) is another build, which it replaces.
    let own = std::env::current_exe().expect("this test's path");
    let exact = Client::connect_exact(home.path(), Some(&own)).await.expect("this release's own");
    let started = Client::connect(home.path(), Some(Path::new("/nowhere/isothrum")))
        .await
        .expect("the running one, nothing started");
    assert_eq!((exact.service().pid, started.service().pid), (info.pid, info.pid));

    let asked = std::time::Instant::now();
    running.shutdown().await.expect("shut down");
    common::ended(serving).await.expect("the service ended");
    // Its open connections (`exact`'s, `started`'s) close with it: none is left to be cut after
    // the 5 s the service gives them.
    assert!(asked.elapsed() < std::time::Duration::from_secs(5), "{:?}", asked.elapsed());
    let after = exact.list().await.expect_err("its service is gone");
    assert_eq!(after.code(), ErrorCode::Io, "{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_followed_operation_reads_its_own_events_to_its_end_and_a_wait_its_outcome() {
    let (_home, _serving, client) = started().await.expect("a service answering");

    let mut followed = client.follow(up("stack")).await.expect("followed");
    assert_eq!((followed.accepted.run.as_str(), followed.accepted.position), ("stack", 0));
    let mut words = Vec::new();
    while let Some(event) = tokio::time::timeout(common::PATIENCE, followed.next()).await.expect("in time") {
        let event = event.expect("an event");
        assert_eq!(event.operation, Some(followed.accepted.operation), "only its own: {event:?}");
        words.push(word(&event));
    }
    assert_eq!(words, ["queued", "started", "log working", "result true", "ended"]);

    let accepted = client.submit(up("stack")).await.expect("queued");
    let outcome = client.wait(accepted.operation).await.expect("it ends well");
    assert_eq!(word(&outcome), "result true");
    assert_eq!(
        client.status(&RunName::new("stack").expect("a name"), None).await.expect("a status").state,
        RunState::Ready
    );
    let listed = client.operations(&RunName::new("stack").expect("a name"), 1).await.expect("its operations");
    assert_eq!(
        listed.iter().map(|o| o.id).collect::<Vec<_>>(),
        [accepted.operation],
        "the newest, as many as asked"
    );
}

/// An operation whose sending went unanswered (the connection ended before the service
/// answered) may have been queued: following it again follows the one queued, from its `queued`,
/// rather than queuing a second. Once that one ended, following again queues anew.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_operation_followed_again_is_the_one_queued_not_a_second() {
    let (_home, _serving, client) = started().await.expect("a service answering");
    let run = RunName::new("held").expect("a name");
    let named = || Command::Up {
        target: Target::new(Path::new("/stacks/held"), Some(run.clone())),
        options: OpOptions::default(),
        lifetime_s: None,
    };
    let first = client.submit(named()).await.expect("queued");
    let mut again = client.follow_again(named()).await.expect("followed again");
    assert_eq!((again.accepted.operation, again.accepted.position), (first.operation, 0), "the one queued");
    HELD.notify_one();
    let mut words = Vec::new();
    while let Some(event) = tokio::time::timeout(common::PATIENCE, again.next()).await.expect("in time") {
        words.push(word(&event.expect("an event")));
    }
    assert_eq!(words, ["queued", "started", "log working", "result true", "ended"]);
    let listed = client.operations(&run, 8).await.expect("its operations");
    assert_eq!(listed.iter().map(|o| o.id).collect::<Vec<_>>(), [first.operation], "none queued twice");

    HELD.notify_one();
    let mut anew = client.follow_again(named()).await.expect("queued anew");
    assert_ne!(anew.accepted.operation, first.operation, "the first ended: a new one");
    assert_eq!(word(&anew.outcome().await.expect("it ends well")), "result true");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_failure_comes_back_with_its_code_and_typed_detail() {
    let (_home, _serving, client) = started().await.expect("a service answering");
    let gone = Detail::PartlyGone {
        machines: vec!["web".to_owned()],
    };

    let refused = client.submit(up("refused")).await.expect_err("refused before it is queued");
    let ClientError::Refused(refusal) = &refused else { panic!("{refused:?}") };
    assert_eq!(
        (refusal.code, refusal.detail.as_ref().map(|d| (d.v, d.data.clone())), refusal.changed),
        (ErrorCode::NothingToResume, Some((2, gone.clone())), false)
    );

    let failed = client.follow(up("gone")).await.expect("queued").outcome().await.expect_err("it fails");
    let ClientError::Operation(failure) = &failed else { panic!("{failed:?}") };
    assert_eq!((failure.code, failure.step.as_deref()), (ErrorCode::NothingToResume, Some("resume")));
    assert_eq!(failure.detail.as_ref().map(|d| d.data.clone()), Some(gone));
    assert_eq!(failed.code(), ErrorCode::NothingToResume);

    let not_running = client.status(&RunName::new("other").expect("a name"), None).await.expect_err("no status");
    assert_eq!(not_running.code(), ErrorCode::NotRunning, "{not_running}");
    let no_variables = client.variables(Target::new("/stacks/stack", None)).await.expect_err("this engine reads none");
    assert_eq!(no_variables.code(), ErrorCode::Unsupported, "{no_variables}");
    let unknown = client.cancel(OperationId(999)).await.expect_err("no such operation");
    assert_eq!(unknown.code(), ErrorCode::Usage, "{unknown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_to_one_run_gets_that_runs_events_only_and_resumes_after_a_cursor() {
    let (_home, _serving, client) = started().await.expect("a service answering");
    let stack = RunName::new("stack").expect("a name");

    client.follow(up("other")).await.expect("followed").outcome().await.expect("other ends well");
    let mut events = client.subscribe(Some(stack.clone()), None).await.expect("subscribed");
    let ran = client.follow(up("stack")).await.expect("followed").outcome().await.expect("stack ends well");
    let mut seen = Vec::new();
    let mut last = None;
    while seen.last().map(String::as_str) != Some("ended") {
        let (cursor, event) = tokio::time::timeout(common::PATIENCE, events.next())
            .await
            .expect("in time")
            .expect("open")
            .expect("an event");
        assert_eq!(event.run.as_deref(), Some("stack"), "{event:?}");
        seen.push(word(&event));
        last = Some(cursor);
    }
    assert!(seen.contains(&word(&ran)), "{seen:?}");

    client.follow(up("stack")).await.expect("followed").outcome().await.expect("stack again");
    let mut after = client.subscribe(Some(stack), last).await.expect("resumed");
    let (_, first) = tokio::time::timeout(common::PATIENCE, after.next())
        .await
        .expect("in time")
        .expect("open")
        .expect("an event");
    assert_eq!(
        phase_of(&first),
        Some(OperationPhase::Queued),
        "the next operation's first event, nothing before the cursor: {first:?}"
    );
}
