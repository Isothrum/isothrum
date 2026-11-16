//! A run left moving by an operation whose process died (an `up` killed midway).

use super::gone::lab;
use super::vagrant::{a_modelled_vagrant_lab, vagrant_answer};
use super::*;
use crate::state::{Desired, Op, Transition};
use crate::store::{Owner, RunRecord};

/// `run` recorded `creating`, its `up` begun by `owner` and never ended.
fn creating(runner: &Runner, run: &crate::Running<'_>, owner: &Owner) {
    creating_as(runner, run, owner, Format::Compose);
}

/// [`creating`], for a project of `format`.
fn creating_as(runner: &Runner, run: &crate::Running<'_>, owner: &Owner, format: Format) {
    let name = run.name().clone();
    let record = RunRecord::fresh(name.clone(), run.project().dir.clone(), format);
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

/// Vagrant over VirtualBox, for a Vagrantfile of `web` and `controller`: whether `web`'s VM runs
/// (the controller's is never made but by an `up` that ends well); the `up`s seen.
#[derive(Debug, Default)]
struct TwoVms {
    web: bool,
    ups: usize,
}

fn two_vms(lab: &Mutex<TwoVms>, call: &Call) -> Reply {
    vagrant_answer(lab, call, |lab, args| match args {
        ["up", "--provider", "virtualbox", ..] => {
            lab.ups += 1;
            lab.web = true;
            Reply::ok("")
        }
        ["status", "--machine-readable"] => {
            let (web, controller) = if lab.ups > 0 {
                ("running", "running")
            } else if lab.web {
                ("running", "not_created")
            } else {
                ("not_created", "not_created")
            };
            Reply::ok(&format!("1794305819,controller,state,{controller}\n1794305819,web,state,{web}"))
        }
        ["port", m, "--machine-readable"] => Reply::ok(&format!("1794305819,{m},forwarded_port,22,2222")),
        _ => panic!("a call the model doesn't know: {}", call.line()),
    })
}

#[tokio::test]
async fn a_vagrant_up_killed_once_a_vm_runs_reads_failed_not_ready_and_the_next_up_takes_it_over() {
    let (lab, _fake, _root, project, runner) = a_modelled_vagrant_lab(two_vms);
    let run = runner.open(&project, None).expect("opened");
    let opts = RunOptions::new().with_provider(crate::Provider::Virtualbox);
    // What a start killed while Vagrant provisioned web leaves: web's VM running, unprovisioned,
    // the controller's never made (`not_created`, which doesn't count), and the run `creating`.
    lab.lock().unwrap_or_else(PoisonError::into_inner).web = true;
    let dead = Owner {
        pid: crate::store::dead_pid(),
        ..Owner::this_process()
    };
    creating_as(&runner, &run, &dead, Format::Vagrant);

    let status = run.status_with(&opts).await.expect("status");
    assert_eq!(
        status.state,
        RunState::Failed,
        "a VM running says nothing of the start's end: {:?}",
        status.machines
    );
    assert_eq!(run.status_with(&opts).await.expect("status").state, RunState::Failed, "and stays so");

    run.up(&opts, &Recorder::new()).await.expect("an up takes it over, never already_running");
    assert_eq!(run.status_with(&opts).await.expect("status").state, RunState::Ready);
}
