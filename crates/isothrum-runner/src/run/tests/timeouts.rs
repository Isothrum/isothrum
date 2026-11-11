//! A step's time limit through a real `up`: the time it shows no progress, not its total time;
//! and the images an `up` starts, pulled before it in a step of their own, with no limit while
//! they move.

use super::*;
use crate::{Error, ErrorCode, StepState, TimedOut};

const MINUTE: Duration = Duration::from_secs(60);

/// The engine, the project's image downloading while it prints `lines`, `pace` apart, once: in
/// the pull before `up`, else in `up` itself (Compose pulls what `up` starts and doesn't have).
fn downloading(engine: &Mutex<Engine>, call: &Call, lines: impl Iterator<Item = String>, pace: Duration) -> Reply {
    let line = call.line();
    let pulls = line.contains(" pull --policy ");
    let ups = line.contains(" up --detach ");
    let downloads = {
        let mut e = engine.lock().unwrap_or_else(PoisonError::into_inner);
        let downloads = (pulls || ups) && e.images == Images::Missing;
        if downloads {
            e.images = Images::Here;
        }
        downloads
    };
    if !downloads {
        return answer(engine, call);
    }
    let mut reply = if pulls { Reply::ok("") } else { answer(engine, call) };
    reply.stdout = lines.chain(reply.stdout).collect();
    reply.paced(pace)
}

/// A slow download: a new line every 10 minutes for 7 hours, past the ceiling every step stops at.
fn slow(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let lines = (1..=42).map(|n| format!("36b0bafd7f97 Downloading {}MB", n * 100));
    downloading(engine, call, lines, 10 * MINUTE)
}

/// A stuck download: two new lines, then the last one again every minute, for a day.
fn stuck(engine: &Mutex<Engine>, call: &Call) -> Reply {
    let lines = ["36b0bafd7f97 Downloading 1.282GB", "36b0bafd7f97 Downloading 1.283GB"]
        .into_iter()
        .chain(std::iter::repeat_n("36b0bafd7f97 Downloading 1.283GB", 24 * 60))
        .map(str::to_owned);
    downloading(engine, call, lines, MINUTE)
}

/// A start of `web-portal` on the engine answering `answer`, which fails: the fixture, the error,
/// the events, and how long the start took.
async fn failed_up(answer: fn(&Mutex<Engine>, &Call) -> Reply) -> (Fixture, Error, Vec<EventKind>, Duration) {
    let f = fixture_answering(WEB_PORTAL, answer);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let started = tokio::time::Instant::now();
    let err = run.up(&opts(), &events).await.unwrap_err();
    let took = started.elapsed();
    (f, err, kinds(&events), took)
}

#[tokio::test(start_paused = true)]
async fn a_slow_pull_still_moving_runs_as_long_as_it_moves() {
    // Regression: an 11 GB attack box pulled through a slow connection was still downloading
    // when the start's `up` step was stopped. Its images are now pulled before it, in a step of
    // their own with no limit while they move, past the six hours any step stops at.
    let f = fixture_answering(WEB_PORTAL, slow);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    let started = tokio::time::Instant::now();
    run.up(&opts(), &events).await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(7 * 3600), "past the ceiling: {:?}", started.elapsed());
    assert_eq!(f.containers(), Containers::Up);
    let pull = f.fake.lines().into_iter().find(|l| l.contains(" pull ")).expect("pulled before `up`");
    assert_eq!(
        pull,
        format!(
            "docker compose --project-name web-portal --file compose.yml --file {} pull --policy missing --ignore-buildable --include-deps db isoloom-routes-db web isoloom-routes-web isoloom-probe-amd64",
            f.home().join("runs/web-portal/ports.yml").display()
        )
    );
    let steps: Vec<(String, StepState)> = kinds(&events)
        .into_iter()
        .filter_map(|k| match k {
            EventKind::Step { step, state, .. } if step.ends_with("up") => Some((step, state)),
            _ => None,
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("pull-up".to_owned(), StepState::Running),
            ("pull-up".to_owned(), StepState::Done),
            ("up".to_owned(), StepState::Running),
            ("up".to_owned(), StepState::Done),
        ]
    );
    assert!(kinds(&events).contains(&EventKind::Log {
        step: "pull-up".into(),
        stream: crate::Stream::Stdout,
        line: "36b0bafd7f97 Downloading 4200MB".into(),
    }));
}

#[tokio::test(start_paused = true)]
async fn a_stuck_pull_is_stopped_as_a_stalled_download() {
    let (f, err, mut events, took) = failed_up(stuck).await;
    // Its last new line came at two minutes: 15 minutes later it is stopped, `up` never run.
    assert_eq!(took, 17 * MINUTE);
    assert_eq!(f.called(" up --detach "), 0);
    assert_eq!(
        err.to_string(),
        "step `pull-up` showed no progress downloading for 900s and was stopped; its last line: `36b0bafd7f97 Downloading 1.283GB`"
    );
    assert!(matches!(&err, Error::DownloadStalled { step, after_s: 900, .. } if step == "pull-up"));
    let Some(EventKind::Error { code, step, timeout, .. }) = events.pop() else {
        panic!()
    };
    assert_eq!((code, step.as_deref(), timeout), (ErrorCode::DownloadStalled, Some("pull-up"), None));
}

#[tokio::test(start_paused = true)]
async fn a_pull_that_fails_says_composes_reason_and_starts_nothing() {
    fn refused(engine: &Mutex<Engine>, call: &Call) -> Reply {
        if call.line().contains(" pull --policy ") {
            return Reply::fail(18, "Error response from daemon: toomanyrequests: You have reached your pull rate limit");
        }
        answer(engine, call)
    }
    let (f, err, mut events, _) = failed_up(refused).await;
    let Error::DownloadFailed { step, reason } = &err else { panic!("{err:?}") };
    assert_eq!(step, "pull-up");
    assert!(reason.contains("toomanyrequests"), "{reason}");
    assert_eq!(f.called(" up --detach "), 0);
    let Some(EventKind::Error { code, .. }) = events.pop() else { panic!() };
    assert_eq!(code, ErrorCode::DownloadFailed);
}

#[tokio::test(start_paused = true)]
async fn ups_own_limit_still_holds_once_its_images_are_pulled() {
    // The images pulled in two hours; then `up` waits on a container, printing nothing.
    fn then_quiet(engine: &Mutex<Engine>, call: &Call) -> Reply {
        if call.line().contains(" up --detach ") {
            return answer(engine, call).after(Duration::from_secs(3 * 3600));
        }
        let lines = (1..=12).map(|n| format!("36b0bafd7f97 Downloading {}MB", n * 100));
        downloading(engine, call, lines, 10 * MINUTE)
    }
    let (_, err, events, took) = failed_up(then_quiet).await;
    assert_eq!(took, Duration::from_secs(2 * 3600 + 1800), "`up`'s 30 minutes, from its start");
    assert!(
        matches!(&err, Error::Timeout { step, timed_out: TimedOut::NoProgress { after_s: 1800, last_line: None } } if step == "up"),
        "{err:?}"
    );
    assert!(events.contains(&EventKind::Step {
        step: "pull-up".into(),
        state: StepState::Done,
        note: None
    }));
}

#[tokio::test(start_paused = true)]
async fn a_compose_without_pull_policy_pulls_in_up_as_before() {
    // A Compose that doesn't know `pull --policy` refuses it: its `up` pulls, under its limit,
    // which counts the time without progress.
    fn old_compose(engine: &Mutex<Engine>, call: &Call) -> Reply {
        if call.line().contains(" pull --policy ") {
            return Reply::fail(16, "unknown flag: --policy");
        }
        let lines = (1..=40).map(|n| format!("36b0bafd7f97 Downloading {}MB/1.121GB", n * 28));
        downloading(engine, call, lines, MINUTE)
    }
    let f = fixture_answering(WEB_PORTAL, old_compose);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = tokio::time::Instant::now();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(started.elapsed() >= 40 * MINUTE, "past the step's 30 minutes");
    assert_eq!(f.containers(), Containers::Up);
}
