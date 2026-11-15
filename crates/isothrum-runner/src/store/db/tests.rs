use std::time::Duration;

use crate::project::Format;
use crate::project::platform::Protocol;

use super::*;
use crate::events::{Event, EventKind, StepState};
use crate::name::RunName;
use crate::state::{Desired, Op, RunState, Transition};
use crate::store::{LastObserved, RunRecord};

fn name(n: &str) -> RunName {
    RunName::new(n).unwrap()
}

fn record(n: &str) -> RunRecord {
    RunRecord::fresh(name(n), "/work/x".into(), Format::Compose)
}

fn store() -> (tempfile::TempDir, Store) {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    (home, store)
}

/// Run `n`, recorded absent, and an operation `op` on it begun by this process.
fn run_with_operation(store: &Store, n: &str, op: Op) -> OperationRow {
    store
        .write(|tx| {
            tx.save_record(&record(n), Desired::Running)?;
            tx.begin_operation(&name(n), op, &Owner::this_process(), None)
        })
        .unwrap()
}

#[test]
fn a_new_home_is_migrated_once_in_wal_mode_and_owner_only() {
    let (home, store) = store();
    let mode: String = store.conn.lock().unwrap().query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!(mode, "wal");
    let fk: i64 = store.conn.lock().unwrap().query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
    assert_eq!(fk, 1);
    drop(store);
    let again = Store::open(home.path()).unwrap();
    let versions: i64 = again
        .conn
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(versions, i64::try_from(schema::MIGRATIONS.len()).unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_mode = std::fs::metadata(home.path().join(DB_FILE)).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600);
    }
}

#[test]
fn a_store_a_newer_runner_wrote_is_refused() {
    let (home, store) = store();
    store
        .conn
        .lock()
        .unwrap()
        .execute("INSERT INTO schema_migrations (version, applied_at) VALUES (99, 'later')", [])
        .unwrap();
    drop(store);
    let err = Store::open(home.path()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::StoreVersion, "{err}");
}

#[test]
fn a_file_that_isnt_a_database_is_store_corrupt() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join(DB_FILE), vec![b'x'; 4096]).unwrap();
    let err = Store::open(home.path()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::StoreCorrupt, "{err}");
}

#[test]
fn a_write_that_fails_leaves_nothing_of_itself() {
    let (_home, store) = store();
    let r = name("r");
    let failed = store.write(|tx| {
        tx.save_record(&record("r"), Desired::Running)?;
        tx.begin_operation(&r, Op::Up, &Owner::this_process(), None)?;
        Err::<(), _>(Error::NotRunning { run: "r".into() })
    });
    assert!(failed.is_err());
    store
        .read(|tx| {
            assert_eq!(tx.run(&r)?, None);
            assert_eq!(tx.operations(&r, 10)?, []);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_record_saved_keeps_its_state_which_only_a_transition_moves() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    store
        .write(|tx| {
            let begun = Transition::begin(RunState::Absent, Op::Up, op.id)?;
            tx.apply(&r, begun)?;
            let mut changed = record("r");
            changed.state = RunState::Ready;
            changed.host_label = Some("a".into());
            tx.save_record(&changed, Desired::Running)?;
            let row = tx.run(&r)?.unwrap();
            assert_eq!((row.state, row.record.state), (RunState::Creating, RunState::Creating));
            assert_eq!(row.record.host_label.as_deref(), Some("a"));
            assert_eq!(row.state_op, Some(op.id));
            tx.apply(&r, Transition::succeed(begun))?;
            assert_eq!(
                tx.transitions(&r)?,
                [
                    (RunState::Absent, RunState::Creating, Some(op.id)),
                    (RunState::Creating, RunState::Ready, Some(op.id))
                ]
            );
            assert!(tx.run(&r)?.unwrap().converged());
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_transition_from_where_the_run_no_longer_stands_is_a_conflict() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Park);
    // The row is absent; a park begun from `ready` is legal but stale.
    let err = store
        .write(|tx| tx.apply(&r, Transition::begin(RunState::Ready, Op::Park, op.id)?))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::StateConflict, "{err}");
    assert_eq!(err.to_string(), "run `r` was expected ready but is absent");
}

#[test]
fn an_illegal_transition_never_reaches_the_store() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Park);
    let err = store
        .write(|tx| tx.apply(&r, Transition::begin(RunState::Absent, Op::Park, op.id)?))
        .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IllegalTransition, "{err}");
    assert_eq!(store.read(|tx| tx.run(&r)).unwrap().unwrap().state, RunState::Absent);
}

#[test]
fn an_observation_moves_a_settled_run_and_never_a_moving_one() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    let seen = LastObserved::default();
    store
        .write(|tx| {
            // Absent, observed ready: not an observation's move (only an `up` makes a run).
            assert_eq!(tx.observe(&r, &seen, RunState::Ready)?, None);
            let begun = Transition::begin(RunState::Absent, Op::Up, op.id)?;
            tx.apply(&r, begun)?;
            assert_eq!(tx.observe(&r, &seen, RunState::Failed)?, None, "an up owns it");
            tx.apply(&r, Transition::succeed(begun))?;
            let moved = tx.observe(&r, &seen, RunState::Failed)?.unwrap();
            assert_eq!(
                (moved.from(), moved.to(), moved.cause()),
                (RunState::Ready, RunState::Failed, Some(ErrorCode::NotReady))
            );
            let row = tx.run(&r)?.unwrap();
            assert_eq!((row.state, row.failed_cause, row.state_op), (RunState::Failed, Some(ErrorCode::NotReady), None));
            assert!(row.observed_at.is_some());
            Ok(())
        })
        .unwrap();
}

/// A crash between `creating` and `created`: the row written ahead is committed, the provider's
/// answer never recorded. The next operation finds the unfinished one and what it may have made.
#[test]
fn a_crash_between_creating_and_created_leaves_the_resource_to_find() {
    let (home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    let made = store
        .write(|tx| {
            let id = tx.creating(&NewResource::new(&r, "web", ResourceKind::Container, op.id).living(Lifetime::Machines))?;
            tx.created(id, "c0ffee")?;
            Ok(id)
        })
        .unwrap();
    let key = "k7";
    let pending = CopyName::rendered(format!("r-{key}-db-1"), key).unwrap();
    store
        .write(|tx| {
            tx.creating(
                &NewResource::new(&r, "db", ResourceKind::Container, op.id)
                    .living(Lifetime::Machines)
                    .named(pending.clone()),
            )
        })
        .unwrap();
    // The provider call ran; the process died before `created` was written.
    drop(store);

    let reopened = Store::open(home.path()).unwrap();
    let unfinished = reopened.read(|tx| tx.unfinished(&r)).unwrap();
    assert_eq!(unfinished.iter().map(|o| o.id).collect::<Vec<_>>(), [op.id]);
    let left = reopened.read(|tx| tx.resources_of(op.id)).unwrap();
    assert_eq!(left.len(), 2);
    assert_eq!(
        (left[0].id, left[0].state, left[0].native_id.as_deref()),
        (made, ResourceState::Created, Some("c0ffee"))
    );
    assert_eq!((left[1].state, left[1].native_id.as_deref()), (ResourceState::Creating, None));
    let found = reopened.read(|tx| tx.resource_named(&r, "db", &pending)).unwrap().unwrap();
    assert_eq!(found.id, left[1].id, "a pending row is found by its per-copy name");
    assert!(CopyName::rendered("db-1", key).is_err(), "never by a bare name");
}

#[test]
fn a_write_that_fails_between_creating_and_created_keeps_neither() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    let failed = store.write(|tx| {
        let id = tx.creating(&NewResource::new(&r, "web", ResourceKind::Container, op.id).living(Lifetime::Run))?;
        tx.created(id, "c0ffee")?;
        Err::<(), _>(Error::NotRunning { run: "r".into() })
    });
    assert!(failed.is_err());
    assert_eq!(store.read(|tx| tx.live_resources(&r)).unwrap(), []);
}

#[test]
fn a_resource_moves_ahead_of_its_calls_and_never_back() {
    let (_home, store) = store();
    let r = name("r");
    let up = run_with_operation(&store, "r", Op::Up);
    let down = store.write(|tx| tx.begin_operation(&r, Op::Down, &Owner::this_process(), None)).unwrap();
    store
        .write(|tx| {
            let id = tx.creating(&NewResource::new(&r, "net", ResourceKind::Network, up.id).living(Lifetime::Run))?;
            tx.created(id, "n1")?;
            tx.created(id, "n1")?;
            tx.destroying(id, down.id)?;
            assert_eq!(tx.created(id, "n2").unwrap_err().code(), ErrorCode::IllegalTransition);
            tx.gone(id)?;
            tx.gone(id)?;
            assert_eq!(tx.destroying(id, down.id).unwrap_err().code(), ErrorCode::IllegalTransition);
            assert_eq!(tx.live_resources(&r)?, []);
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_local_kind_keeps_its_own_lifetime() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    store
        .write(|tx| {
            let keys = tx.creating(&NewResource::new(&r, "known_hosts", ResourceKind::HostKeys, op.id))?;
            assert_eq!(tx.live_resources(&r)?[0].lifetime, Lifetime::Machines);
            let wrong = tx.creating(&NewResource::new(&r, "known_hosts", ResourceKind::HostKeys, op.id).living(Lifetime::Run));
            assert_eq!(wrong.unwrap_err().code(), ErrorCode::IllegalTransition);
            let unsaid = tx.creating(&NewResource::new(&r, "vm", ResourceKind::Vm, op.id));
            assert_eq!(unsaid.unwrap_err().code(), ErrorCode::IllegalTransition, "a provider kind's lifetime is said");
            tx.gone(keys)
        })
        .unwrap();
}

#[test]
fn the_journal_keeps_each_operations_steps_end_and_cancel() {
    let (_home, store) = store();
    let r = name("r");
    let op = run_with_operation(&store, "r", Op::Up);
    store
        .write(|tx| {
            tx.step(op.id, "up", StepState::Running)?;
            tx.step(op.id, "up", StepState::Done)?;
            assert!(tx.request_cancel(op.id)?);
            tx.end_operation(op.id, Outcome::Cancelled, Some(ErrorCode::Cancelled))?;
            tx.end_operation(op.id, Outcome::Succeeded, None)?;
            assert!(!tx.request_cancel(op.id)?, "ended");
            Ok(())
        })
        .unwrap();
    store
        .read(|tx| {
            assert_eq!(tx.steps(op.id)?, [("up".to_owned(), StepState::Running), ("up".to_owned(), StepState::Done)]);
            let row = tx.operation(op.id)?.unwrap();
            assert!(row.cancel_requested);
            assert_eq!(row.ended.map(|e| e.outcome), Some(Outcome::Cancelled), "the first end stands");
            assert_eq!(tx.unfinished(&r)?, []);
            Ok(())
        })
        .unwrap();
}

#[test]
fn events_are_appended_in_order_and_read_after_a_cursor() {
    let (_home, store) = store();
    let (a, b) = (name("a"), name("b"));
    let event = |run: &RunName, seq| {
        Event::new(
            seq,
            Some(run.to_string()),
            Some(Op::Up),
            EventKind::Result {
                ok: true,
                data: crate::ResultData::Moved { state: crate::RunState::Ready },
            },
        )
    };
    let first = store.write(|tx| tx.append_event(None, &event(&a, 1))).unwrap();
    let second = store.write(|tx| tx.append_event(None, &event(&b, 1))).unwrap();
    let third = store.write(|tx| tx.append_event(None, &event(&a, 2))).unwrap();
    assert!(first < second && second < third);
    store
        .read(|tx| {
            let (all, unreadable) = tx.events_after(first, None, 10)?;
            assert_eq!((all.len(), unreadable), (2, 0));
            let (of_a, _) = tx.events_after(EventCursor::default(), Some(&a), 10)?;
            assert_eq!(of_a.iter().map(|(c, e)| (*c, e.seq)).collect::<Vec<_>>(), [(first, 1), (third, 2)]);
            assert_eq!(tx.last_cursor()?, third);
            Ok(())
        })
        .unwrap();
    store.write(|tx| tx.trim_events(&a, 1)).unwrap();
    let (kept, _) = store.read(|tx| tx.events_after(EventCursor::default(), Some(&a), 10)).unwrap();
    assert_eq!(kept.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [third]);
}

#[test]
fn a_queued_operation_is_started_once_or_cancelled_and_never_otherwise_ended() {
    let (_home, store) = store();
    let r = name("r");
    let me = Owner::this_process();
    store
        .write(|tx| {
            let queued = tx.queue_operation(&r, Op::Up, &me)?;
            assert_eq!((queued.started_at.as_deref(), queued.ended.as_ref()), (None, None));
            assert_eq!(tx.unfinished(&r)?.len(), 1, "a queued one is unfinished");
            let failed = tx.end_operation(queued.id, Outcome::Failed, Some(ErrorCode::Timeout));
            assert_eq!(failed.unwrap_err().code(), ErrorCode::IllegalTransition, "it never ran");
            let started = tx.start_operation(queued.id, &r, &me, None)?;
            assert!(started.started_at.is_some());
            let again = tx.start_operation(queued.id, &r, &me, None);
            assert_eq!(again.unwrap_err().code(), ErrorCode::IllegalTransition, "started once");
            let elsewhere = tx.queue_operation(&r, Op::Down, &me)?;
            assert!(tx.start_operation(elsewhere.id, &name("other"), &me, None).is_err(), "another run's");
            tx.end_operation(elsewhere.id, Outcome::Cancelled, Some(ErrorCode::Cancelled))?;
            assert!(tx.start_operation(elsewhere.id, &r, &me, None).is_err(), "a cancelled one never starts");
            Ok(())
        })
        .unwrap();
}

mod crash;

mod leases;

mod queue;

mod robust;
