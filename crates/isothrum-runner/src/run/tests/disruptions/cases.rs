//! Each disruption alone, and the way back from it.

use super::super::opts;
use super::{Ctr, Disruption, fixture};
use crate::status::MachineState;
use crate::{ErrorCode, EventKind, Park, Recorder, RunOptions, RunState, WarningCode};

/// The machine `name`'s state in `status`.
fn machine(status: &crate::Status, name: &str) -> MachineState {
    status.machines.iter().find(|m| m.name == name).map(|m| m.state.clone()).unwrap()
}

/// Whether `events` warned `code`.
fn warned(events: &Recorder, code: WarningCode) -> bool {
    events
        .events()
        .iter()
        .any(|e| matches!(&e.kind, EventKind::Warning { code: c, .. } if *c == code))
}

#[tokio::test]
async fn a_machine_removed_while_the_run_runs_shows_gone_and_a_down_then_up_brings_it_back() {
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.project().ctrs.remove("web");

    let status = run.status().await.unwrap();
    assert_eq!(status.state, RunState::Failed, "a run with a machine gone isn't ready");
    assert_eq!((machine(&status, "web"), machine(&status, "db")), (MachineState::Absent, MachineState::Running));
    // A start of a run whose other machines run is refused, saying it runs: the way back is a
    // `down`, then an `up` (an app's "reset").
    assert_eq!(run.up(&opts(), &Recorder::new()).await.unwrap_err().code(), ErrorCode::AlreadyRunning);
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(f.project().ctrs.is_empty(), "nothing is left: {:?}", f.project().ctrs);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.is_whole(&run).await;
}

#[tokio::test]
async fn a_machine_removed_while_the_start_waits_fails_it_and_the_next_start_clears_what_it_left() {
    let f = fixture();
    f.project().during_up = Some(Disruption::Remove("web"));
    let run = f.run();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed);
    assert!(err.to_string().contains("No such container: web-portal-web-1"), "the engine's words: {err}");
    assert_eq!(f.record().state, RunState::Failed);
    let status = run.status().await.unwrap();
    assert_eq!((status.state, machine(&status, "web")), (RunState::Failed, MachineState::Absent));

    let again = Recorder::new();
    run.up(&opts(), &again).await.unwrap();
    assert!(warned(&again, WarningCode::LeftoversRemoved), "what the failed start left is removed first");
    assert_eq!(f.called(" down --volumes"), 1);
    assert!(f.every_member_runs());
    assert_eq!((f.record().state, run.status().await.unwrap().state), (RunState::Ready, RunState::Ready));
}

#[tokio::test]
async fn a_start_whose_process_was_killed_is_failed_not_ready_and_the_next_start_clears_it() {
    // Regression: a start killed after its containers came up (SIGKILL, a crash, the computer
    // off) left its record `starting`. Its run read as ready though its set-up never ran, and the
    // next start was refused as already running.
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.killed_in(RunState::Starting);

    let status = run.status().await.unwrap();
    assert_eq!(status.state, RunState::Failed, "a start that never ended isn't ready");
    assert!(status.message.is_none(), "nor says where to begin");

    let again = Recorder::new();
    run.up(&opts(), &again).await.unwrap();
    assert!(warned(&again, WarningCode::LeftoversRemoved));
    let first = again.events().into_iter().find_map(|e| match e.kind {
        EventKind::State { from, to } => Some((from, to)),
        _ => None,
    });
    assert_eq!(
        first,
        Some((RunState::Failed, RunState::Starting)),
        "it starts from the failed run the killed one left"
    );
    assert!(f.every_member_runs());
    let record = f.record();
    assert_eq!(record.state, RunState::Ready);
    assert!(record.last.is_some_and(|l| l.ok && l.op == crate::Op::Up));
}

#[tokio::test]
async fn any_operation_killed_midway_leaves_a_failed_run_that_a_down_removes() {
    for left in [RunState::Starting, RunState::Stopping, RunState::Parking, RunState::Resuming] {
        let f = fixture();
        let run = f.run();
        run.up(&opts(), &Recorder::new()).await.unwrap();
        f.killed_in(left);
        assert_eq!(run.status().await.unwrap().state, RunState::Failed, "{left}");
        // The next operation takes the run's lock: the record of the killed one is settled
        // under it, whatever that operation is (a check changes nothing of the run).
        run.check(&opts(), &Recorder::new()).await.ok();
        assert_eq!(f.record().state, RunState::Failed, "{left}");
        run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
        assert!(f.project().ctrs.is_empty(), "{left}: {:?}", f.project().ctrs);
        assert_eq!(f.record().state, RunState::Absent);
    }
}

#[tokio::test]
async fn a_down_that_cant_run_after_a_killed_operation_leaves_the_run_failed_with_its_own_failure() {
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.killed_in(RunState::Parking);
    f.project().uninstalled = true;
    assert_eq!(run.down(&RunOptions::new(), &Recorder::new()).await.unwrap_err().code(), ErrorCode::ToolMissing);
    let record = f.record();
    assert_eq!(record.state, RunState::Failed);
    let last = record.last.unwrap();
    assert_eq!((last.op, last.ok, last.code), (crate::Op::Down, false, Some(ErrorCode::ToolMissing)));
}

#[tokio::test]
async fn a_resume_with_a_machine_removed_while_parked_is_refused_and_the_next_start_rebuilds() {
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    f.project().ctrs.remove("web");
    let starts = f.called(" start");

    crate::testing::refuses_to_resume_without(&run, "web").await;
    assert_eq!(f.called(" start"), starts, "nothing was started without it");
    assert_eq!(f.project().state("db"), Some(Ctr::Exited));
    assert_eq!(f.record().state, RunState::Failed);
    assert_eq!(run.status().await.unwrap().state, RunState::Failed);

    let again = Recorder::new();
    run.up(&opts(), &again).await.unwrap();
    assert!(warned(&again, WarningCode::LeftoversRemoved));
    assert!(f.every_member_runs());
}

#[tokio::test]
async fn a_start_that_resumes_parked_runs_rebuilds_one_with_a_machine_removed() {
    // An embedder whose start resumes a parked run (an app's Start on a shut-down lab) gets a
    // fresh run, not a refusal, when a machine of it was removed meanwhile.
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    f.project().ctrs.remove("db");
    let resuming = opts().with_occupied(crate::Occupied::ResumeParked);
    let events = Recorder::new();
    run.up(&resuming, &events).await.unwrap();
    assert!(warned(&events, WarningCode::LeftoversRemoved));
    assert_eq!(f.called(" down --volumes"), 1);
    assert!(f.every_member_runs());
    assert_eq!(f.record().state, RunState::Ready);
}

#[tokio::test]
async fn a_resume_with_a_machine_stopped_outside_brings_it_back() {
    // `docker stop` of one container: it is still there, so a resume starts it again.
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.project().ctrs.insert("web", Ctr::Exited);
    let status = run.status().await.unwrap();
    assert_eq!((status.state, machine(&status, "web")), (RunState::Failed, MachineState::Exited(0)));
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    f.is_whole(&run).await;
}

#[tokio::test]
async fn a_down_of_a_run_whose_containers_were_all_removed_outside_ends_absent() {
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.project().ctrs.clear();
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
    // Nothing to park: refused, and the next start is a fresh one.
    assert_eq!(
        run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap_err().code(),
        ErrorCode::NotRunning
    );
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.record().state, RunState::Absent);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(f.every_member_runs());
}

#[tokio::test]
async fn the_daemon_stopped_while_the_start_waits_fails_it_with_the_engines_words_and_a_start_once_it_is_back_works() {
    let f = fixture();
    f.project().during_up = Some(Disruption::StopDaemon);
    let run = f.run();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolFailed);
    assert!(err.to_string().contains("Cannot connect to the Docker daemon"), "{err}");
    assert_eq!(f.record().state, RunState::Failed);
    // While it is down the status can't be read: an error, the record left as it was.
    assert!(run.status().await.is_err());
    assert_eq!(f.record().state, RunState::Failed);

    f.project().daemon_down = false;
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(f.every_member_runs());
    assert_eq!(f.record().state, RunState::Ready);
}

#[tokio::test]
async fn docker_uninstalled_after_the_start_refuses_and_a_down_once_it_is_back_still_works() {
    let f = fixture();
    let run = f.run();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.project().uninstalled = true;
    assert_eq!(run.status().await.unwrap_err().code(), ErrorCode::ToolMissing);
    assert_eq!(run.down(&RunOptions::new(), &Recorder::new()).await.unwrap_err().code(), ErrorCode::ToolMissing);
    assert!(!f.record().state.is_moving(), "never left mid-way: {:?}", f.record().state);
    assert!(f.every_member_runs(), "nothing was removed");

    f.project().uninstalled = false;
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(f.project().ctrs.is_empty());
    assert_eq!(f.record().state, RunState::Absent);
}

#[tokio::test]
async fn a_lock_file_a_dead_process_left_doesnt_hold_the_run() {
    // The OS lock goes with the process that held it; its file stays, and holds nothing.
    let f = fixture();
    let run = f.run();
    let lock = f.runner.store.lock_path(run.name());
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(f.every_member_runs());
}
