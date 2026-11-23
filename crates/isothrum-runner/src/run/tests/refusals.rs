//! What a run refuses before any tool acts: an `up` of a run that is up and live, a
//! `resume` of a run of which nothing is left, a change of a run that isn't up.

use super::*;

/// The code of the `error` event `events` ended with.
fn error_code(events: &Recorder) -> Option<ErrorCode> {
    events.events().into_iter().find_map(|e| match e.kind {
        EventKind::Error(report) => Some(report.code),
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

/// Another process acting on the run (its lock held by another handle, as across processes): an
/// operation fails `locked`, says so in its error event, journals nothing and runs no tool; once
/// let go, the run is this runner's again.
#[tokio::test(start_paused = true)]
async fn an_operation_on_a_run_another_process_holds_is_locked_and_runs_no_tool() {
    let (_root, project, runner, _engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    runner.store.run_dir(run.name()).expect("the run's folder");
    let other = std::fs::File::create(runner.store.lock_path(run.name())).expect("the lock file");
    other.lock().expect("held by the other process");
    let before = fake.calls().len();
    let events = Recorder::new();
    let e = run.up(&RunOptions::new(), &events).await.expect_err("locked");
    assert_eq!(e.code(), ErrorCode::Locked, "{e}");
    assert_eq!(error_code(&events), Some(ErrorCode::Locked));
    assert_eq!(fake.calls().len(), before, "no tool ran");
    let journaled = runner.store.db().read(|tx| tx.operations(run.name(), 10)).expect("the journal");
    assert_eq!(journaled, [], "nothing journaled");
    drop(other);
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up once let go");
}

#[tokio::test]
async fn a_change_of_a_run_that_isnt_up_is_not_running_and_runs_no_tool() {
    let (_root, project, runner, _engine, fake) = setup_seen();
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new();
    let refused = |e: crate::Error| {
        assert_eq!((e.code(), e.report().changed), (ErrorCode::NotRunning, false), "{e}");
    };
    // Never started.
    refused(run.update(&opts, &Recorder::new()).await.expect_err("never started"));
    refused(run.exec("web", &["true".to_owned()], &opts, &Recorder::new()).await.expect_err("never started"));
    refused(run.attach("web", &opts, &Recorder::new()).await.expect_err("never started"));
    assert_eq!(fake.calls().len(), 0, "no tool ran");
    // Parked: an update would start its machines, a command its services.
    run.up(&opts, &Recorder::new()).await.expect("up");
    run.park(crate::Park::Shutdown, &opts, &Recorder::new()).await.expect("parked");
    let before = fake.calls().len();
    let events = Recorder::new();
    refused(run.update(&opts, &events).await.expect_err("parked"));
    assert_eq!(error_code(&events), Some(ErrorCode::NotRunning));
    refused(run.retire("web", &opts, &Recorder::new()).await.expect_err("parked"));
    refused(run.provision(None, None, &opts, &Recorder::new()).await.expect_err("parked"));
    assert_eq!(fake.calls().len(), before, "no tool ran");
    assert_eq!(run.status().await.expect("status").state, RunState::Parked, "and the run is as it was");
}
