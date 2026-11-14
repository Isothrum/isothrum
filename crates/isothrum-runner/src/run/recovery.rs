//! What an operation does about the resources a dead one left: the rows its process wrote
//! `creating`/`created` (or `destroying`) and never saw through, removed by the id the tool gave,
//! never by a name. And the events that say each move of the run's resource rows.

use crate::Result;
use crate::error::Error;
use crate::events::Emitter;
use crate::store::{OperationId, ResourceId, ResourceRow, Store};

/// Says, as `Resource` events, where the rows `ids` stand now (a row the store no longer has is
/// skipped). A store that can't be read is traced: the rows moved, only the saying failed.
pub(crate) fn say(events: &Emitter<'_>, db: &Store, ids: &[ResourceId]) {
    let rows = db.read(|tx| ids.iter().map(|id| tx.resource(*id)).collect::<Result<Vec<_>>>());
    match rows {
        Ok(rows) => rows.iter().flatten().for_each(|row| events.resource(row)),
        Err(e) => tracing::warn!(error = %e, "the run's resource moves weren't said"),
    }
}

/// A row recovery couldn't remove, and why: it stays `destroying` for the next operation.
#[derive(Debug)]
pub(crate) struct NotRemoved {
    /// The row.
    pub(crate) row: ResourceRow,
    /// Why.
    pub(crate) error: Error,
}

/// What removes a recorded resource by the id its tool gave (the format driver's
/// `destroy_by_id`). A trait, not a closure: an operation's future stays `Send` whatever the
/// remover borrows.
pub(crate) trait Remover {
    /// Removes `row`'s resource; nothing there any more is fine.
    async fn remove(&mut self, row: &ResourceRow) -> Result<()>;
}

/// Removes `rows` (newest first, as [`crate::store::Tx::left_by_dead_operations`] gives them) for
/// operation `by`: each is written `destroying` before `remover` removes it by the row's recorded
/// id and `gone` after it, each move said on `events`. A removal that fails leaves its row
/// `destroying`, and the next operation tries again; the others go on.
///
/// # Errors
///
/// The store's, when a row's move can't be written: nothing is removed past it.
pub(crate) async fn destroy_recorded(
    db: &Store,
    events: &Emitter<'_>,
    by: OperationId,
    rows: &[ResourceRow],
    mut remover: impl Remover,
) -> Result<Vec<NotRemoved>> {
    let mut not_removed = Vec::new();
    for row in rows {
        db.write(|tx| tx.destroying(row.id, by))?;
        say(events, db, &[row.id]);
        match remover.remove(row).await {
            Ok(()) => {
                db.write(|tx| tx.gone(row.id))?;
                say(events, db, &[row.id]);
            }
            Err(error) => {
                tracing::warn!(key = %row.key, error = %error, "a resource a dead operation left couldn't be removed; the next operation tries again");
                not_removed.push(NotRemoved { row: row.clone(), error });
            }
        }
    }
    Ok(not_removed)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::clock::FixedClock;
    use crate::events::{EventKind, Recorder, StoreLog};
    use crate::name::RunName;
    use crate::state::Op;
    use crate::store::{Lifetime, NewResource, Outcome, Owner, ResourceKind, ResourceState};

    /// A store with run `r`: an `up` whose process died after it made `vm-a` (created) and was
    /// making `vm-b` (`creating`, no id yet), a finished `up` that made `net` (still live), and
    /// the current operation.
    fn left_by_a_dead_up() -> (tempfile::TempDir, Arc<Store>, RunName, OperationId) {
        let home = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(home.path()).unwrap());
        let r = RunName::new("r").unwrap();
        let mut gone = Owner::this_process();
        gone.pid = u32::MAX;
        gone.boot = "a boot before this one".into();
        let me = Owner::this_process();
        let current = store
            .write(|tx| {
                let done = tx.begin_operation(&r, Op::Up, &me, None)?;
                let net = tx.creating(&NewResource::new(&r, "network:lab", ResourceKind::Network, done.id).living(Lifetime::Run))?;
                tx.created(net, "net-1")?;
                tx.end_operation(done.id, Outcome::Succeeded, None)?;
                let dead = tx.begin_operation(&r, Op::Up, &gone, None)?;
                let a = tx.creating(&NewResource::new(&r, "vm:a", ResourceKind::Vm, dead.id).living(Lifetime::Machines))?;
                tx.created(a, "uuid-a")?;
                tx.creating(&NewResource::new(&r, "vm:b", ResourceKind::Vm, dead.id).living(Lifetime::Machines))?;
                let now = tx.begin_operation(&r, Op::Up, &me, Some(dead.id))?;
                tx.end_operation(dead.id, Outcome::Abandoned, Some(crate::ErrorCode::Cancelled))?;
                Ok(now.id)
            })
            .unwrap();
        (home, store, r, current)
    }

    #[test]
    fn a_dead_operations_rows_with_an_id_are_what_recovery_removes() {
        let (_home, store, r, current) = left_by_a_dead_up();
        let left = store.read(|tx| tx.left_by_dead_operations(&r, current)).unwrap();
        let keys: Vec<&str> = left.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["vm:a"], "not the finished up's network, nor a row with no id yet");
    }

    #[tokio::test]
    async fn recovery_removes_by_recorded_id_says_each_move_and_keeps_what_failed() {
        let (_home, store, r, current) = left_by_a_dead_up();
        let mut extra = Owner::this_process();
        extra.pid = u32::MAX;
        extra.boot = "a boot before this one".into();
        // A second dead operation's VM, whose removal fails.
        store
            .write(|tx| {
                let dead = tx.begin_operation(&r, Op::Update, &extra, None)?;
                let c = tx.creating(&NewResource::new(&r, "vm:c", ResourceKind::Vm, dead.id).living(Lifetime::Machines))?;
                tx.created(c, "uuid-c")
            })
            .unwrap();
        let rows = store.read(|tx| tx.left_by_dead_operations(&r, current)).unwrap();
        let clock = FixedClock(std::time::UNIX_EPOCH);
        let sink = Recorder::new();
        let events = Emitter::new("r", Op::Up, &clock, &sink).with_log(StoreLog::new(Arc::clone(&store)));
        /// Removes each row asked for, but `vm:c`, and notes the ids it was asked.
        struct Asked<'a>(&'a mut Vec<String>);
        impl Remover for Asked<'_> {
            async fn remove(&mut self, row: &ResourceRow) -> Result<()> {
                self.0.push(row.native_id.clone().unwrap_or_default());
                if row.key == "vm:c" {
                    return Err(Error::NotRunning { run: "r".into() });
                }
                Ok(())
            }
        }
        let mut asked = Vec::new();
        let failed = destroy_recorded(&store, &events, current, &rows, Asked(&mut asked)).await.unwrap();
        assert_eq!(asked, ["uuid-c", "uuid-a"], "newest first, by the tool's own id");
        assert_eq!(failed.iter().map(|n| n.row.key.as_str()).collect::<Vec<_>>(), ["vm:c"]);
        assert_eq!(failed[0].error.code(), crate::ErrorCode::NotRunning, "with why");
        let states: Vec<(String, ResourceState)> = sink
            .events()
            .into_iter()
            .filter_map(|e| match e.kind {
                EventKind::Resource { key, state, .. } => Some((key, state)),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            [
                ("vm:c".to_owned(), ResourceState::Destroying),
                ("vm:a".to_owned(), ResourceState::Destroying),
                ("vm:a".to_owned(), ResourceState::Gone),
            ]
        );
        let live: Vec<String> = store.read(|tx| tx.live_resources(&r)).unwrap().into_iter().map(|row| row.key).collect();
        assert_eq!(
            live,
            ["network:lab", "vm:b", "vm:c"],
            "the failed one stays, destroying, for the next operation"
        );
        // The next recovery finds it again (its operation is still dead), and never the gone one.
        let again: Vec<String> = store
            .read(|tx| tx.left_by_dead_operations(&r, current))
            .unwrap()
            .into_iter()
            .map(|row| row.key)
            .collect();
        assert_eq!(again, ["vm:c"]);
    }
}
