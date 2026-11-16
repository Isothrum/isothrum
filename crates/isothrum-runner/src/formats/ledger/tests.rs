use super::*;
use crate::state::{Desired, Op};
use crate::store::{Lifetime, Owner, ResourceKind, Store};

fn found(key: &str, kind: ResourceKind, id: &str) -> Found {
    Found {
        key: key.to_owned(),
        kind,
        native_id: id.to_owned(),
        lifetime: Lifetime::Machines,
    }
}

/// A store with run `r` and an operation `op` on it, begun by this process.
fn with_operation(op: Op) -> (tempfile::TempDir, Store, RunName, OperationId) {
    let home = tempfile::tempdir().expect("a home");
    let store = Store::open(home.path()).expect("a store");
    let run = RunName::new("r").expect("a name");
    let record = crate::store::RunRecord::fresh(run.clone(), home.path().to_path_buf(), crate::project::Format::Compose);
    let by = store
        .write(|tx| {
            tx.save_record(&record, Desired::Running)?;
            tx.begin_operation(&run, op, &Owner::this_process(), None)
        })
        .expect("an operation")
        .id;
    (home, store, run, by)
}

/// Records `found` as [`record_found`] does, with the keys of the rows it made and of those it
/// marked gone (read before and after: a gone row is no longer live).
fn record(store: &Store, run: &RunName, by: OperationId, found: &[Found]) -> (Vec<String>, Vec<String>) {
    let before = store.read(|tx| tx.live_resources(run)).expect("read");
    let recorded = store.write(|tx| record_found(tx, run, by, found)).expect("recorded");
    let after = store.read(|tx| tx.live_resources(run)).expect("read");
    let key = |id: &crate::store::ResourceId| before.iter().chain(&after).find(|r| r.id == *id).map(|r| r.key.clone());
    (recorded.made.iter().filter_map(key).collect(), recorded.gone.iter().filter_map(key).collect())
}

fn states(store: &Store, run: &RunName) -> Vec<(String, ResourceState, Option<String>)> {
    store
        .read(|tx| tx.live_resources(run))
        .expect("read")
        .into_iter()
        .map(|r| (r.key, r.state, r.native_id))
        .collect()
}

#[test]
fn what_the_tool_reports_is_recorded_once_with_its_id() {
    let (_home, store, run, by) = with_operation(Op::Up);
    let seen = [
        found("container:web-1", ResourceKind::Container, "c0ffee"),
        found("network:stack", ResourceKind::Network, "n1"),
    ];
    let (made, _) = record(&store, &run, by, &seen);
    assert_eq!(made, ["container:web-1", "network:stack"]);
    let again = store.write(|tx| record_found(tx, &run, by, &seen)).expect("recorded");
    assert_eq!(again, Recorded::default(), "the same report records nothing more");
    assert_eq!(states(&store, &run).len(), 2);
}

#[test]
fn a_resource_the_tool_no_longer_reports_is_gone_and_its_re_creation_is_a_new_row() {
    let (_home, store, run, by) = with_operation(Op::Up);
    store
        .write(|tx| record_found(tx, &run, by, &[found("container:web-1", ResourceKind::Container, "old")]))
        .expect("recorded");
    let (made, gone) = record(&store, &run, by, &[found("container:web-1", ResourceKind::Container, "new")]);
    assert_eq!((made, gone), (vec!["container:web-1".to_owned()], vec!["container:web-1".to_owned()]));
    assert_eq!(
        states(&store, &run),
        [("container:web-1".to_owned(), ResourceState::Created, Some("new".to_owned()))]
    );
}

#[test]
fn a_row_written_ahead_of_the_call_gets_the_id_the_tool_reports() {
    let (_home, store, run, by) = with_operation(Op::Up);
    store
        .write(|tx| tx.creating(&NewResource::new(&run, "vm:web", ResourceKind::Vm, by).living(Lifetime::Machines)))
        .expect("written ahead");
    let (made, _) = record(&store, &run, by, &[found("vm:web", ResourceKind::Vm, "uuid-1")]);
    assert_eq!(made, ["vm:web"]);
    assert_eq!(states(&store, &run), [("vm:web".to_owned(), ResourceState::Created, Some("uuid-1".to_owned()))]);
}

#[test]
fn only_rows_with_an_id_are_left_to_remove_and_a_row_with_none_is_never_gone_by_absence() {
    let (_home, store, run, by) = with_operation(Op::Up);
    store
        .write(|tx| {
            tx.creating(&NewResource::new(&run, "vm:half", ResourceKind::Vm, by).living(Lifetime::Machines))?;
            record_found(tx, &run, by, &[found("vm:web", ResourceKind::Vm, "uuid-1")])
        })
        .expect("recorded");
    let left = store.read(|tx| left_to_remove(tx, &run)).expect("read");
    assert_eq!(left.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["vm:web"]);
    let (_, gone) = record(&store, &run, by, &[]);
    assert_eq!(gone, ["vm:web"], "the half-made row keeps waiting for its id");
}
