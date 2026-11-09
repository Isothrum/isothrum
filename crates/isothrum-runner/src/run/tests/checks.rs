//! The run's checks through the fake engine, under Tokio's paused clock (a runner's wait passes at
//! once).

use super::*;
use crate::CheckReport;

/// The run started, then checked: the report and the check's events.
async fn started_and_checked(f: &Fixture) -> (CheckReport, Recorder) {
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let events = Recorder::new();
    let report = run.check(&RunOptions::default(), &events).await.unwrap();
    (report, events)
}

#[tokio::test(start_paused = true)]
async fn every_check_reported_passes_and_is_an_event() {
    let f = fixture();
    let (report, events) = started_and_checked(&f).await;
    assert!(report.ok, "{report:?}");
    assert_eq!(report.results.len(), 7);
    assert_eq!(report.unfinished, Vec::<String>::new());
    let checks = kinds(&events).into_iter().filter(|k| matches!(k, EventKind::Check { .. })).count();
    assert_eq!(checks, 7);
    let derived = f.fake.last("isoloom-check-db").unwrap();
    assert_eq!(derived.env("ISOLOOM_DERIVED"), Some("1"), "delivered to the check runners");
}

#[tokio::test(start_paused = true)]
async fn a_runner_waiting_longer_than_five_minutes_runs_to_its_end() {
    // Regression (R11): a cap of 5 minutes on every check runner was shorter than the
    // checks' own waits (up to 1500 s in the corpus); the manifest's timeout (465 s here) holds.
    let f = fixture();
    f.check_takes("isoloom-check", Duration::from_secs(400));
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let report = run.check(&RunOptions::default(), &Recorder::new()).await.unwrap();
    assert!(report.ok, "{report:?}");
}

#[tokio::test(start_paused = true)]
async fn a_runner_past_its_timeout_is_stopped_cleaned_up_and_its_checks_fail() {
    let f = fixture();
    f.check_takes("isoloom-check-db", Duration::from_secs(121));
    let (report, events) = started_and_checked(&f).await;
    assert!(!report.ok);
    assert_eq!(report.unfinished, ["isoloom-check-db"]);
    let db: Vec<_> = report.results.iter().filter(|r| r.runner == "isoloom-check-db").collect();
    assert!(
        db.len() == 2 && db.iter().all(|r| !r.ok && r.detail == "the runner stopped before this check"),
        "{db:?}"
    );
    assert!(
        f.called("label=com.docker.compose.service=isoloom-check-db --filter label=com.docker.compose.oneoff=True") > 0,
        "on_timeout ran"
    );
    assert!(matches!(kinds(&events).last(), Some(EventKind::Result { ok: false, .. })));
}

#[tokio::test(start_paused = true)]
async fn checks_need_a_running_run() {
    let f = fixture();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.check(&RunOptions::default(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), crate::ErrorCode::NotRunning);
}
