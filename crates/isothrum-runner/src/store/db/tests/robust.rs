//! The store's robustness, as NTFS keeps a volume: the WAL is the journal (a process killed
//! mid-transaction leaves no trace of it), the integrity check at open, and the mirror of the
//! critical rows a damaged store is rebuilt from.

use std::io::{Seek as _, SeekFrom, Write as _};

use super::*;
use crate::store::{Lifetime, NewResource, ResourceKind, ResourceState};

/// The variable that makes [`a_child_killed_mid_transaction`] write in the home it names, and
/// hang inside its transaction until it is killed.
const CHILD_HOME: &str = "ISOTHRUM_TEST_STORE_CHILD_HOME";

/// Run `web`, up, with a VM `vm:web` made (its tool id `uuid-web`) by an `up` still open.
fn a_run_with_a_vm(store: &Store) -> OperationId {
    let r = name("web");
    store
        .write(|tx| {
            tx.save_record(&record("web"), Desired::Running)?;
            let op = tx.begin_operation(&r, Op::Up, &Owner::this_process(), None)?;
            let vm = tx.creating(&NewResource::new(&r, "vm:web", ResourceKind::Vm, op.id).living(Lifetime::Machines))?;
            tx.created(vm, "uuid-web")?;
            Ok(op.id)
        })
        .unwrap()
}

/// Not a test of its own: the child of [`a_process_killed_mid_transaction_leaves_no_trace_of_it`],
/// run only when that test sets [`CHILD_HOME`].
#[test]
fn a_child_killed_mid_transaction() {
    let Some(home) = std::env::var_os(CHILD_HOME) else { return };
    let store = Store::open(std::path::Path::new(&home)).unwrap();
    // The transaction never ends: the parent kills the process inside it.
    drop(store.write(|tx| -> crate::Result<()> {
        tx.save_record(&record("half"), Desired::Running)?;
        println!("inside");
        std::io::stdout().flush().unwrap();
        loop {
            std::thread::park();
        }
    }));
}

#[test]
fn a_process_killed_mid_transaction_leaves_no_trace_of_it() {
    let home = tempfile::tempdir().unwrap();
    a_run_with_a_vm(&Store::open(home.path()).unwrap());
    let (mut child, inside) = crate::testing::child_test_saying("store::db::tests::robust::a_child_killed_mid_transaction", CHILD_HOME, home.path(), "inside");
    // SIGKILL on Unix, TerminateProcess on Windows: no handler runs, the transaction is cut.
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(inside, "the child was inside its transaction");
    let store = Store::open(home.path()).expect("the store opens, checked");
    assert_eq!(store.recovered(), None, "nothing to repair: the WAL dropped the cut transaction");
    let runs: Vec<String> = store
        .read(|tx| tx.runs().map(|rows| rows.into_iter().map(|r| r.name.to_string()).collect()))
        .unwrap();
    assert_eq!(runs, ["web"], "the committed run, not the half-written one");
    store
        .write(|tx| tx.save_record(&record("after"), Desired::Running))
        .expect("and takes writes again");
}

/// Damages the store's file at `path`: its header and first pages overwritten.
fn damage(path: &std::path::Path) {
    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(&[0xA5; 8192]).unwrap();
    f.sync_all().unwrap();
}

/// A home whose store holds [`a_run_with_a_vm`] (the operation's id beside it), its mirror
/// written, and its WAL checkpointed into the main file, so damaging the file damages the rows.
fn a_home_with_a_vm() -> (tempfile::TempDir, OperationId) {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    let op = a_run_with_a_vm(&store);
    store.conn.lock().unwrap().execute_batch("PRAGMA wal_checkpoint(TRUNCATE)").unwrap();
    drop(store);
    (home, op)
}

#[test]
fn a_damaged_store_is_rebuilt_from_its_mirror_and_kept_aside() {
    let (home, op) = a_home_with_a_vm();
    assert!(home.path().join(MIRROR_FILE).is_file(), "the mirror follows each committed change");
    damage(&home.path().join(DB_FILE));

    let rebuilt = Store::open(home.path()).expect("rebuilt from the mirror");
    let recovered = rebuilt.recovered().expect("says it repaired");
    assert!(recovered.corrupt_kept_at.is_file(), "the damaged file kept for diagnostics");
    let row = rebuilt.read(|tx| tx.run(&name("web"))).unwrap().expect("the run is back");
    assert_eq!((row.state, row.desired), (RunState::Absent, Desired::Running));
    let unfinished = rebuilt.read(|tx| tx.unfinished(&name("web"))).unwrap();
    assert_eq!(
        unfinished.iter().map(|o| o.id).collect::<Vec<_>>(),
        [op],
        "the open operation, for the next one to recover"
    );
    // And the store works on: a write, and a new mirror.
    rebuilt.write(|tx| tx.save_record(&record("after"), Desired::Running)).unwrap();
    assert!(rebuilt.read(|tx| tx.run(&name("after"))).unwrap().is_some());
}

#[tokio::test]
async fn a_resource_recorded_in_the_mirror_is_still_removed_by_its_id() {
    let (home, _) = a_home_with_a_vm();
    damage(&home.path().join(DB_FILE));

    let rebuilt = std::sync::Arc::new(Store::open(home.path()).unwrap());
    assert!(rebuilt.recovered().is_some());
    let r = name("web");
    let next = rebuilt.write(|tx| tx.begin_operation(&r, Op::Down, &Owner::this_process(), None)).unwrap();
    let left = rebuilt.read(|tx| tx.left_by_dead_operations(&r, next.id)).unwrap();
    assert_eq!(left.iter().map(|row| row.native_id.as_deref()).collect::<Vec<_>>(), [Some("uuid-web")]);
    let clock = crate::clock::FixedClock(std::time::UNIX_EPOCH);
    let sink = crate::events::Recorder::new();
    let events = crate::events::Emitter::new("web", Op::Down, &clock, &sink);
    /// Notes the id of each row it is asked to remove.
    struct Noted<'a>(&'a mut Vec<Option<String>>);
    impl crate::run::recovery::Remover for Noted<'_> {
        async fn remove(&mut self, row: &ResourceRow) -> crate::Result<()> {
            self.0.push(row.native_id.clone());
            Ok(())
        }
    }
    let mut removed = Vec::new();
    let failed = crate::run::recovery::destroy_recorded(&rebuilt, &events, next.id, &left, Noted(&mut removed))
        .await
        .unwrap();
    assert!(failed.is_empty());
    assert_eq!(removed, [Some("uuid-web".to_owned())], "by the tool's own id");
    let vm = rebuilt.read(|tx| tx.resource(left[0].id)).unwrap().unwrap();
    assert_eq!(vm.state, ResourceState::Gone);
}

#[test]
fn an_emptied_store_beside_a_mirror_is_rebuilt_not_taken_as_new() {
    let (home, _) = a_home_with_a_vm();
    std::fs::File::create(home.path().join(DB_FILE)).unwrap();
    let rebuilt = Store::open(home.path()).unwrap();
    assert!(rebuilt.recovered().is_some(), "truncated to nothing: repaired");
    assert!(rebuilt.read(|tx| tx.run(&name("web"))).unwrap().is_some());
}

#[test]
fn a_damaged_store_with_no_mirror_is_a_typed_error() {
    let (home, _) = a_home_with_a_vm();
    std::fs::remove_file(home.path().join(MIRROR_FILE)).unwrap();
    damage(&home.path().join(DB_FILE));
    let err = Store::open(home.path()).unwrap_err();
    assert_eq!(err.code(), ErrorCode::StoreCorrupt, "{err}");
    assert!(err.to_string().contains("no mirror to rebuild it from"), "{err}");
    assert!(home.path().join(DB_FILE).is_file(), "left where it was, for whoever looks at it");
}

#[test]
fn the_mirror_is_rewritten_only_when_its_rows_change() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::open(home.path()).unwrap();
    a_run_with_a_vm(&store);
    let mirror = home.path().join(MIRROR_FILE);
    let before = std::fs::read(&mirror).unwrap();
    // Events and observations aren't mirrored: the file stays as it was.
    store
        .write(|tx| {
            tx.append_raw_event(Some("web"), None, r#"{"v":1}"#)?;
            tx.keep_observed(&name("web"), &LastObserved::default()).map(drop)
        })
        .unwrap();
    assert_eq!(std::fs::read(&mirror).unwrap(), before);
    store.write(|tx| tx.set_desired(&name("web"), Desired::Gone)).unwrap();
    assert_ne!(std::fs::read(&mirror).unwrap(), before, "a run's change is");
}
