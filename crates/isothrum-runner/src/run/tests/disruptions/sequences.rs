//! Disruptions and operations in any order: what holds after each, and that the run always
//! comes back whole.

use proptest::prelude::*;

use super::super::{SECRET, opts};
use super::{Ctr, Disruption, Fixture, MEMBERS, fixture};
use crate::{EventKind, Park, Recorder, RunOptions, RunState};

/// Something done to the run: an operation through the runner, or a disruption outside it.
#[derive(Debug, Clone, Copy)]
enum Act {
    Up,
    Down,
    Park,
    Resume,
    Status,
    /// `docker rm -f` of a member's container.
    Remove(usize),
    /// `docker stop` of a member's container.
    Stop(usize),
    /// Every container of the project removed.
    RemoveAll,
    /// The next start's wait sees a member's container removed.
    RemoveDuringUp(usize),
    /// The daemon stopped, or started again.
    Daemon(bool),
    /// The process running an operation killed, its record left in this moving state.
    Killed(usize),
}

fn act() -> impl Strategy<Value = Act> {
    prop_oneof![
        3 => Just(Act::Up),
        2 => Just(Act::Down),
        1 => Just(Act::Park),
        1 => Just(Act::Resume),
        1 => Just(Act::Status),
        1 => (0..MEMBERS.len()).prop_map(Act::Remove),
        1 => (0..MEMBERS.len()).prop_map(Act::Stop),
        1 => Just(Act::RemoveAll),
        1 => (0..MEMBERS.len()).prop_map(Act::RemoveDuringUp),
        1 => any::<bool>().prop_map(Act::Daemon),
        1 => (0..4usize).prop_map(Act::Killed),
    ]
}

/// The moving states a killed operation leaves.
const MOVING: [RunState; 4] = [RunState::Starting, RunState::Stopping, RunState::Parking, RunState::Resuming];

/// Does `act` to the run of `f`, then holds what must hold after it.
async fn apply(f: &Fixture, run: &crate::Running<'_>, act: Act) -> std::result::Result<(), TestCaseError> {
    let events = Recorder::new();
    let done = match act {
        Act::Up => run.up(&opts(), &events).await.map(|_| ()),
        Act::Down => run.down(&RunOptions::new(), &events).await,
        Act::Park => run.park(Park::Shutdown, &RunOptions::new(), &events).await,
        Act::Resume => run.resume(&RunOptions::new(), &events).await.map(|_| ()),
        Act::Status => {
            if let Ok(status) = run.status().await {
                // A ready run is one every machine of which runs.
                prop_assert!(status.state != RunState::Ready || f.every_member_runs(), "ready with {:?}", f.project().ctrs);
            }
            return Ok(());
        }
        outside => {
            disrupt(f, run, outside);
            return Ok(());
        }
    };
    // Every operation ends with exactly one `result` or `error`, and never leaves the record
    // mid-way.
    let ends = events
        .events()
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Result { .. } | EventKind::Error { .. }))
        .count();
    prop_assert_eq!(ends, 1, "{:?}: {:?}", act, events.events());
    if let Some(record) = f.runner.store.record(run.name()).unwrap() {
        prop_assert!(!record.state.is_moving(), "{:?} left the record {}", act, record.state);
    }
    if done.is_ok() {
        match act {
            // A run that came up (or back) is one every machine of which runs.
            Act::Up | Act::Resume => prop_assert!(f.every_member_runs(), "{:?} ok with {:?}", act, f.project().ctrs),
            Act::Down => prop_assert!(f.project().ctrs.is_empty(), "down ok with {:?}", f.project().ctrs),
            _ => {}
        }
    }
    Ok(())
}

/// Does `act`, something done outside the runner, to the project of `f`'s `run`.
fn disrupt(f: &Fixture, run: &crate::Running<'_>, act: Act) {
    let mut project = f.project();
    match act {
        Act::Remove(m) => {
            project.ctrs.remove(MEMBERS[m]);
        }
        Act::Stop(m) => {
            if let Some(c) = project.ctrs.get_mut(MEMBERS[m]) {
                *c = Ctr::Exited;
            }
        }
        Act::RemoveAll => project.ctrs.clear(),
        Act::RemoveDuringUp(m) => project.during_up = Some(Disruption::Remove(MEMBERS[m])),
        Act::Daemon(up) => project.daemon_down = !up,
        Act::Killed(s) => {
            drop(project);
            if f.runner.store.record(run.name()).unwrap().is_some() {
                f.killed_in(MOVING[s]);
            }
        }
        Act::Up | Act::Down | Act::Park | Act::Resume | Act::Status => unreachable!("{act:?} is an operation"),
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    /// Whatever a user does to a run, through the runner or outside it, in whatever order: every
    /// operation ends once and never leaves its record mid-way, what reads ready is ready, and
    /// once the daemon runs again a `down` removes everything and an `up` brings the whole run
    /// back.
    #[test]
    fn whatever_is_done_to_a_run_it_comes_back_whole(acts in proptest::collection::vec(act(), 1..14)) {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            let f = fixture();
            let run = f.run();
            for act in acts {
                apply(&f, &run, act).await?;
            }
            {
                let mut p = f.project();
                p.daemon_down = false;
                p.during_up = None;
            }
            prop_assert!(run.down(&RunOptions::new(), &Recorder::new()).await.is_ok());
            prop_assert!(f.project().ctrs.is_empty(), "{:?}", f.project().ctrs);
            prop_assert_eq!(run.status().await.unwrap().state, RunState::Absent);
            let up = run.up(&opts(), &Recorder::new()).await;
            prop_assert!(up.is_ok(), "{:?}", up.err());
            prop_assert!(f.every_member_runs());
            prop_assert_eq!(run.status().await.unwrap().state, RunState::Ready);
            prop_assert!(!format!("{:?}", f.fake.lines()).contains(SECRET), "a delivered value never lands on a command line");
            Ok(())
        })?;
    }
}
