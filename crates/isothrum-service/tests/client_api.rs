//! The client's public API as a launcher calls it, against a real service on a fresh home: the
//! three ways in, an operation followed to its end and waited for, the subscriptions, and each
//! failure with its code and typed detail. The service's engine is the test's own, so what each
//! call gets back is known; the service's own side is `round_trip.rs` and `service.rs`. It lives
//! here, not in the client's `tests/`: the service depends on the client, and the workspace keeps
//! its dependencies one way.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use isothrum_client::proto::{Command, OpOptions, PROTOCOL, Target};
use isothrum_client::v1::{Event, EventKind, OperationPhase, phase_of};
use isothrum_client::{Client, ClientError, VERSION};
use isothrum_runner::store::OperationId;
use isothrum_runner::{BoxFuture, Detail, ErrorCode, ResultData, RunName, RunState, Status};
use isothrum_service::{Engine, Journal, Refusal, ServeError, ServeOptions, Service, StopSignal};

/// What a helper's failure fails the test with.
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// The failure every run named `gone` has: its machine `web` removed outside it.
fn web_gone() -> isothrum_runner::Error {
    isothrum_runner::Error::PartlyGone {
        run: "gone".to_owned(),
        machines: vec!["web".to_owned()],
    }
}

/// An engine for runs named after their folder: `refused` is refused before anything is queued;
/// an operation on `gone` fails at its step `resume`, any other ends well after one line of log.
/// Only `lab` has a status.
struct Lab;

impl Engine for Lab {
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
                let error = EventKind::Error {
                    code: e.code(),
                    message: e.to_string(),
                    step: Some("resume".to_owned()),
                    timeout: None,
                    detail: e.detail(),
                };
                journal.keep(Event::new(1, run, op, error));
                return false;
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
            if run.as_str() != "lab" {
                return Err(Refusal::new(ErrorCode::NotRunning, format!("`{run}` isn't running")));
            }
            let ready = serde_json::json!({ "state": "ready", "provider": null, "machines": [], "outputs": [] });
            serde_json::from_value(ready).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
        })
    }
}

/// A service on `home` over [`Lab`], never stopped but by a request.
fn serve(home: &Path) -> Outcome<tokio::task::JoinHandle<Result<(), ServeError>>> {
    let never: StopSignal = Box::new(|| Box::pin(std::future::pending()));
    let options = ServeOptions::new(Arc::new(Lab), never);
    Ok(tokio::spawn(Service::hold(home)?.serve(options)))
}

/// A client of `home`'s service once it answers.
async fn answering(home: &Path) -> Outcome<Client> {
    for _ in 0..200 {
        if let Ok(client) = Client::connect_running(home).await {
            return Ok(client);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Err("the service didn't answer".into())
}

/// A service on a fresh home, its task, and a client of it.
async fn started() -> Outcome<(tempfile::TempDir, tokio::task::JoinHandle<Result<(), ServeError>>, Client)> {
    let home = tempfile::tempdir()?;
    let serving = serve(home.path())?;
    let client = answering(home.path()).await?;
    Ok((home, serving, client))
}

/// An `up` of the run in the folder named `run`.
fn up(run: &str) -> Command {
    Command::Up {
        target: Target::new(Path::new("/labs").join(run), None),
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
        (EventKind::Error { code, .. }, None) => format!("error {code:?}"),
        (other, None) => format!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_finds_no_service_then_the_one_running_by_each_way_in() {
    let home = tempfile::tempdir().expect("a home");
    let none = Client::connect_running(home.path()).await.expect_err("nothing runs yet");
    assert!(matches!(&none, ClientError::Unavailable(why) if why.contains("no service runs for")), "{none}");
    assert_eq!(none.code(), ErrorCode::Io);

    let serving = serve(home.path()).expect("a service");
    let running = answering(home.path()).await.expect("it answers");
    let info = running.service().clone();
    assert_eq!((info.protocol, info.version.as_str()), (PROTOCOL, VERSION));
    let exact = Client::connect_exact(home.path(), None).await.expect("this release's own");
    let started = Client::connect(home.path(), Some(Path::new("/nowhere/isothrum")))
        .await
        .expect("the running one, nothing started");
    assert_eq!((exact.service().pid, started.service().pid), (info.pid, info.pid));

    running.shutdown().await.expect("shut down");
    // Its open connections close with it, within moments: none waits to be cut.
    let ended = tokio::time::timeout(Duration::from_secs(3), serving).await;
    assert!(matches!(ended, Ok(Ok(Ok(())))), "{ended:?}");
    let after = exact.list().await.expect_err("its service is gone");
    assert_eq!(after.code(), ErrorCode::Io, "{after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_followed_operation_reads_its_own_events_to_its_end_and_a_wait_its_outcome() {
    let (_home, _serving, client) = started().await.expect("a service answering");

    let mut followed = client.follow(up("lab")).await.expect("followed");
    assert_eq!((followed.accepted.run.as_str(), followed.accepted.position), ("lab", 0));
    let mut words = Vec::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(5), followed.next()).await.expect("in time") {
        let event = event.expect("an event");
        assert_eq!(event.operation, Some(followed.accepted.operation), "only its own: {event:?}");
        words.push(word(&event));
    }
    assert_eq!(words, ["queued", "started", "log working", "result true", "ended"]);

    let accepted = client.submit(up("lab")).await.expect("queued");
    let outcome = client.wait(accepted.operation).await.expect("it ends well");
    assert_eq!(word(&outcome), "result true");
    assert_eq!(
        client.status(&RunName::new("lab").expect("a name"), None).await.expect("a status").state,
        RunState::Ready
    );
    let listed = client.operations(&RunName::new("lab").expect("a name"), 1).await.expect("its operations");
    assert_eq!(
        listed.iter().map(|o| o.id).collect::<Vec<_>>(),
        [accepted.operation],
        "the newest, as many as asked"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_failure_comes_back_with_its_code_and_typed_detail() {
    let (_home, _serving, client) = started().await.expect("a service answering");
    let gone = Detail::PartlyGone {
        machines: vec!["web".to_owned()],
    };

    let refused = client.submit(up("refused")).await.expect_err("refused before it is queued");
    let ClientError::Refused { code: why, detail: fields, .. } = &refused else {
        panic!("{refused:?}")
    };
    assert_eq!(
        (*why, fields.as_ref().map(|d| (d.v, d.data.clone()))),
        (ErrorCode::NothingToResume, Some((1, gone.clone())))
    );

    let failed = client.follow(up("gone")).await.expect("queued").outcome().await.expect_err("it fails");
    let ClientError::Operation { code, step, detail, .. } = &failed else {
        panic!("{failed:?}")
    };
    assert_eq!((*code, step.as_deref()), (ErrorCode::NothingToResume, Some("resume")));
    assert_eq!(detail.as_ref().map(|d| d.data.clone()), Some(gone));
    assert_eq!(failed.code(), ErrorCode::NothingToResume);

    let not_running = client.status(&RunName::new("other").expect("a name"), None).await.expect_err("no status");
    assert_eq!(not_running.code(), ErrorCode::NotRunning, "{not_running}");
    let no_variables = client.variables(Target::new("/labs/lab", None)).await.expect_err("this engine reads none");
    assert_eq!(no_variables.code(), ErrorCode::Unsupported, "{no_variables}");
    let unknown = client.cancel(OperationId(999)).await.expect_err("no such operation");
    assert_eq!(unknown.code(), ErrorCode::Usage, "{unknown}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_subscription_to_one_run_gets_that_runs_events_only_and_resumes_after_a_cursor() {
    let (_home, _serving, client) = started().await.expect("a service answering");
    let lab = RunName::new("lab").expect("a name");

    client.follow(up("other")).await.expect("followed").outcome().await.expect("other ends well");
    let mut events = client.subscribe(Some(lab.clone()), None).await.expect("subscribed");
    let ran = client.follow(up("lab")).await.expect("followed").outcome().await.expect("lab ends well");
    let mut seen = Vec::new();
    let mut last = None;
    while seen.last().map(String::as_str) != Some("ended") {
        let (cursor, event) = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("in time")
            .expect("open")
            .expect("an event");
        assert_eq!(event.run.as_deref(), Some("lab"), "{event:?}");
        seen.push(word(&event));
        last = Some(cursor);
    }
    assert!(seen.contains(&word(&ran)), "{seen:?}");

    client.follow(up("lab")).await.expect("followed").outcome().await.expect("lab again");
    let mut after = client.subscribe(Some(lab), last).await.expect("resumed");
    let (_, first) = tokio::time::timeout(Duration::from_secs(5), after.next())
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
