use std::time::Duration;

use isoloom_manifest::{Driver, Protocol, Target};

use super::*;
use crate::events::{Event, EventKind, StepState};
use crate::name::RunName;
use crate::state::{Desired, Op, RunState, Transition};
use crate::store::{LastObserved, RunRecord};

fn name(n: &str) -> RunName {
    RunName::new(n).unwrap()
}

fn record(n: &str) -> RunRecord {
    RunRecord::fresh(name(n), "/work/x/run.json".into(), Target::Docker, Driver::Compose)
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
            changed.params.insert("A".into(), "1".into());
            tx.save_record(&changed, Desired::Running)?;
            let row = tx.run(&r)?.unwrap();
            assert_eq!((row.state, row.record.state), (RunState::Creating, RunState::Creating));
            assert_eq!(row.record.params["A"], "1");
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
                data: serde_json::Value::Null,
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
fn a_lease_is_refused_while_its_holder_lives_and_taken_over_from_a_dead_one() {
    let (_home, store) = store();
    let (me, other) = (Owner::this_process(), Owner::this_process());
    let ttl = Duration::from_secs(60);
    store
        .write(|tx| {
            assert_eq!(tx.acquire_lease("run:r", &me, ttl)?, LeaseOutcome::Acquired { took_over: None });
            assert_eq!(
                tx.acquire_lease("run:r", &me, ttl)?,
                LeaseOutcome::Acquired { took_over: None },
                "its own, renewed"
            );
            assert!(matches!(tx.acquire_lease("run:r", &other, ttl)?, LeaseOutcome::Held(l) if l.owner == me));
            assert!(tx.renew_lease("run:r", &me, ttl)?);
            assert!(!tx.renew_lease("run:r", &other, ttl)?);
            tx.release_lease("run:r", &other)?;
            assert!(tx.lease("run:r")?.is_some(), "another's release takes nothing");
            tx.release_lease("run:r", &me)?;
            assert_eq!(tx.lease("run:r")?, None);
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn a_lease_whose_holder_died_is_taken_over() {
    let (_home, store) = store();
    let mut dead = Owner::this_process();
    dead.pid = owner::test_pids::dead_pid();
    let me = Owner::this_process();
    store
        .write(|tx| {
            tx.acquire_lease("run:r", &dead, Duration::from_secs(3600))?;
            match tx.acquire_lease("run:r", &me, Duration::from_secs(60))? {
                LeaseOutcome::Acquired { took_over: Some(left) } => assert_eq!(left.owner, dead),
                other => panic!("taken over: {other:?}"),
            }
            assert_eq!(tx.lease("run:r")?.unwrap().owner, me);
            Ok(())
        })
        .unwrap();
}

#[test]
fn an_expired_lease_is_taken_over() {
    let (_home, store) = store();
    let (first, second) = (Owner::this_process(), Owner::this_process());
    store
        .write(|tx| {
            tx.acquire_lease("home:slots", &first, Duration::ZERO)?;
            assert!(matches!(
                tx.acquire_lease("home:slots", &second, Duration::from_secs(60))?,
                LeaseOutcome::Acquired { took_over: Some(_) }
            ));
            Ok(())
        })
        .unwrap();
}

#[test]
fn a_port_is_reserved_by_one_live_operation_and_freed_with_its_lease() {
    let (_home, store) = store();
    let (one, two) = (name("one"), name("two"));
    let a = run_with_operation(&store, "one", Op::Up);
    let b = run_with_operation(&store, "two", Op::Up);
    let (me, other) = (Owner::this_process(), Owner::this_process());
    let ttl = Duration::from_secs(60);
    store
        .write(|tx| {
            tx.acquire_lease("run:one", &me, ttl)?;
            tx.acquire_lease("run:two", &other, ttl)?;
            assert!(tx.reserve_port(&Reserver::new(&one, Some(a.id), "run:one", &me), 8080, Protocol::Tcp)?);
            assert!(
                tx.reserve_port(&Reserver::new(&one, Some(a.id), "run:one", &me), 8080, Protocol::Tcp)?,
                "its own again"
            );
            assert!(!tx.reserve_port(&Reserver::new(&two, Some(b.id), "run:two", &other), 8080, Protocol::Tcp)?);
            assert!(
                tx.reserve_port(&Reserver::new(&two, Some(b.id), "run:two", &other), 8080, Protocol::Udp)?,
                "another protocol"
            );
            assert_eq!(tx.port_holder(8080, Protocol::Tcp)?.map(|h| h.run), Some(one.clone()));
            assert_eq!(tx.port_reservations()?.len(), 2);
            tx.release_lease("run:one", &me)?;
            assert!(
                tx.reserve_port(&Reserver::new(&two, Some(b.id), "run:two", &other), 8080, Protocol::Tcp)?,
                "freed with its lease"
            );
            tx.release_ports(b.id)?;
            assert_eq!(tx.port_reservations()?, []);
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn a_port_a_dead_process_reserved_is_taken_over() {
    let (_home, store) = store();
    let (one, two) = (name("one"), name("two"));
    let a = run_with_operation(&store, "one", Op::Up);
    let b = run_with_operation(&store, "two", Op::Up);
    let mut dead = Owner::this_process();
    dead.pid = owner::test_pids::dead_pid();
    let me = Owner::this_process();
    store
        .write(|tx| {
            tx.acquire_lease("run:one", &dead, Duration::from_secs(3600))?;
            assert!(tx.reserve_port(&Reserver::new(&one, Some(a.id), "run:one", &dead), 8080, Protocol::Tcp)?);
            tx.acquire_lease("run:two", &me, Duration::from_secs(60))?;
            assert!(tx.reserve_port(&Reserver::new(&two, Some(b.id), "run:two", &me), 8080, Protocol::Tcp)?);
            Ok(())
        })
        .unwrap();
}

/// A home a 2.x runner wrote: its records, observations, events and recorded volumes become
/// rows once, and its files are left read-only beside the store.
#[test]
fn a_2x_home_is_imported_once_and_its_files_left_read_only() {
    let home = tempfile::tempdir().unwrap();
    let runs = home.path().join("runs");
    let web = runs.join("web");
    std::fs::create_dir_all(&web).unwrap();
    let mut v2 = serde_json::to_value(record("web")).unwrap();
    // 2.x's words for the moving states.
    v2["state"] = "starting".into();
    std::fs::write(web.join("record.json"), serde_json::to_vec(&v2).unwrap()).unwrap();
    std::fs::write(web.join("seen.json"), r#"{"machines":[],"published":[["web","http",8080]],"hosts":{}}"#).unwrap();
    let line = r#"{"v":0,"seq":1,"time":"2026-10-15T12:00:00Z","run":"web","op":"up","event":"state","from":"absent","to":"starting"}"#;
    std::fs::write(web.join("events.jsonl"), format!("{line}\n{{\"v\":0,\"se\n")).unwrap();
    std::fs::write(web.join("anonymous-volumes"), "abc123\ndef456\n").unwrap();
    let parked = runs.join("parked");
    std::fs::create_dir_all(&parked).unwrap();
    let mut p = record("parked");
    p.state = RunState::Parked;
    std::fs::write(parked.join("record.json"), serde_json::to_vec(&p).unwrap()).unwrap();
    let broken = runs.join("broken");
    std::fs::create_dir_all(&broken).unwrap();
    std::fs::write(broken.join("record.json"), "{not json").unwrap();

    let store = Store::open(home.path()).unwrap();
    store
        .read(|tx| {
            let rows = tx.runs()?;
            assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["parked", "web"]);
            let web_row = tx.run(&name("web"))?.unwrap();
            assert_eq!((web_row.state, web_row.desired, web_row.state_op), (RunState::Creating, Desired::Running, None));
            assert_eq!(web_row.observed.unwrap().published, [("web".to_owned(), "http".to_owned(), 8080)]);
            assert_eq!(tx.run(&name("parked"))?.unwrap().desired, Desired::Parked);
            let (events, unreadable) = tx.events_after(EventCursor::default(), Some(&name("web")), 10)?;
            assert_eq!((events.len(), unreadable), (1, 1));
            let volumes: Vec<_> = tx.live_resources(&name("web"))?.into_iter().map(|r| (r.kind, r.native_id.unwrap())).collect();
            assert_eq!(
                volumes,
                [(ResourceKind::Volume, "abc123".to_owned()), (ResourceKind::Volume, "def456".to_owned())]
            );
            Ok(())
        })
        .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(web.join("record.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o400, "left read-only");
        let broken_mode = std::fs::metadata(broken.join("record.json")).unwrap().permissions().mode();
        assert_ne!(broken_mode & 0o777, 0o400, "a record not imported stays as it was");
    }
    // Once: a record put there later isn't imported.
    drop(store);
    let late = runs.join("late");
    std::fs::create_dir_all(&late).unwrap();
    std::fs::write(late.join("record.json"), serde_json::to_vec(&record("late")).unwrap()).unwrap();
    let reopened = Store::open(home.path()).unwrap();
    assert_eq!(reopened.read(|tx| tx.run(&name("late"))).unwrap(), None);
}
