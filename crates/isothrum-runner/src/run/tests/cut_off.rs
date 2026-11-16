//! A run left moving by an operation whose process died (an `up` killed midway).

use super::gone::lab;
use super::*;
use crate::state::{Desired, Op, Transition};
use crate::store::{Owner, RunRecord};

/// `run` recorded `creating`, its `up` begun by `owner` and never ended.
fn creating(runner: &Runner, run: &crate::Running<'_>, owner: &Owner) {
    let name = run.name().clone();
    let record = RunRecord::fresh(name.clone(), run.project().dir.clone(), Format::Compose);
    runner
        .store
        .db()
        .write(|tx| {
            tx.save_record(&record, Desired::Running)?;
            let op = tx.begin_operation(&name, Op::Up, owner, None)?;
            tx.apply(&name, Transition::begin(RunState::Absent, Op::Up, op.id)?)
        })
        .expect("recorded");
}

#[tokio::test]
async fn a_run_whose_ups_process_died_reads_failed_and_comes_up_again() {
    let (_root, project, runner, lab) = lab();
    lab.lock().unwrap_or_else(PoisonError::into_inner).present = vec!["cache"];
    let run = runner.open(&project, None).expect("opened");
    let dead = Owner {
        pid: crate::store::dead_pid(),
        ..Owner::this_process()
    };
    creating(&runner, &run, &dead);

    let status = run.status().await.expect("status");
    assert_eq!(status.state, RunState::Failed, "{:?}", status.machines);
    let record = runner.store.record(run.name()).expect("the store").expect("the record");
    assert_eq!(record.last.map(|l| l.code), Some(Some(ErrorCode::Cancelled)), "its up cut off");
    let open = runner.store.db().read(|tx| tx.unfinished(run.name())).expect("the journal");
    assert!(open.is_empty(), "the dead up ended: {open:?}");

    run.up(&RunOptions::new(), &Recorder::new()).await.expect("an up takes it over");
    assert_eq!(run.status().await.expect("status").state, RunState::Ready);
}

#[tokio::test]
async fn a_run_a_live_process_holds_keeps_its_moving_state() {
    let (_root, project, runner, _lab) = lab();
    let run = runner.open(&project, None).expect("opened");
    creating(&runner, &run, &Owner::this_process());
    assert_eq!(run.status().await.expect("status").state, RunState::Creating);
}
