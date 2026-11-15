//! What a run's state refuses before any tool acts: an `up` of a run that is up and live, a
//! `resume` of a run of which nothing is left.

use super::*;

/// The `docker compose … up` calls made so far.
fn ups(fake: &crate::process::fake::FakeProcesses) -> usize {
    fake.calls().iter().filter(|c| c.args.iter().any(|a| a == "up")).count()
}

/// The code of the `error` event `events` ended with.
fn error_code(events: &Recorder) -> Option<ErrorCode> {
    events.events().into_iter().find_map(|e| match e.kind {
        EventKind::Error { code, .. } => Some(code),
        _ => None,
    })
}

#[tokio::test]
async fn an_up_of_a_ready_live_run_is_already_running_and_starts_nothing() {
    let (_root, project, runner, _engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    let events = Recorder::new();
    let e = run.up(&RunOptions::new(), &events).await.expect_err("up already");
    assert_eq!(e.code(), ErrorCode::AlreadyRunning, "{e}");
    assert_eq!(error_code(&events), Some(ErrorCode::AlreadyRunning));
    assert_eq!(ups(&fake), 1, "the second up started nothing");
    assert_eq!(run.status().await.expect("status").state, RunState::Ready, "and left the run as it was");
}

#[tokio::test]
async fn an_up_of_a_run_recorded_ready_whose_machines_are_gone_starts_it_again() {
    let (_root, project, runner, engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    engine.lock().unwrap_or_else(PoisonError::into_inner).up = false;
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("started again");
    assert_eq!(ups(&fake), 2);
}

#[tokio::test]
async fn a_resume_of_nothing_is_nothing_to_resume_and_runs_no_tool() {
    let (_root, project, runner, _engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    let never = run.resume(&RunOptions::new(), &Recorder::new()).await.expect_err("never started");
    assert_eq!(never.code(), ErrorCode::NothingToResume, "{never}");
    assert_eq!(fake.calls().len(), 0, "no tool ran");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    run.down(&RunOptions::new(), &Recorder::new()).await.expect("down");
    let after_down = run.resume(&RunOptions::new(), &Recorder::new()).await.expect_err("brought down");
    assert_eq!(after_down.code(), ErrorCode::NothingToResume, "{after_down}");
}

#[tokio::test]
async fn a_parked_run_whose_machines_were_removed_has_nothing_to_resume_and_reads_absent() {
    let (_root, project, runner, engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    run.park(crate::Park::Pause, &RunOptions::new(), &Recorder::new()).await.expect("parked");
    engine.lock().unwrap_or_else(PoisonError::into_inner).up = false;
    let e = run.resume(&RunOptions::new(), &Recorder::new()).await.expect_err("nothing left");
    assert_eq!(e.code(), ErrorCode::NothingToResume, "{e}");
    assert_eq!(ups(&fake), 1, "nothing was started again");
    let record = runner.store.record(run.name()).expect("the store").expect("the record");
    assert_eq!(record.state, RunState::Absent, "the parked mark is dropped");
}
