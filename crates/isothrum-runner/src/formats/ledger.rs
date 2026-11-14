//! The resource ledger fed from what the tools report: after a driver's call, the resources its
//! tool says belong to the run ([`Found`]) are diffed against the run's live provider rows. A
//! found one with no row gets one (`creating` then `created`, with its id). A row still
//! `creating` from before the call gets its id. A row whose resource the tool no longer reports is
//! `gone`. Rows are matched by key and kind, and a row's id is only ever the tool's own: nothing is
//! matched by a bare name (#109).

use crate::Result;
use crate::name::RunName;
use crate::store::{NewResource, OperationId, ResourceId, ResourceRow, ResourceState, Scope, Tx};

use super::Found;

/// What [`record_found`] changed: the rows it touched, for the core to emit (`Emitter::resource`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// The rows newly recorded as made, or given their id.
    pub(crate) made: Vec<ResourceId>,
    /// The rows marked gone.
    pub(crate) gone: Vec<ResourceId>,
}

/// Records `found`, all the tool reports of `run`'s resources after a call of operation `by`,
/// against `run`'s live provider rows (see the module).
///
/// # Errors
///
/// The store's (`illegal_transition` when a row can't move as asked).
pub(crate) fn record_found(tx: &mut Tx<'_>, run: &RunName, by: OperationId, found: &[Found]) -> Result<Recorded> {
    let live: Vec<ResourceRow> = tx.live_resources(run)?.into_iter().filter(|r| r.scope == Scope::Provider).collect();
    let mut recorded = Recorded::default();
    for f in found {
        let row = live
            .iter()
            .find(|r| r.key == f.key && r.kind == f.kind && r.native_id.as_deref().is_none_or(|id| id == f.native_id));
        match row {
            // Already recorded, or being removed (a removal still under way keeps its row moving).
            Some(r) if r.state == ResourceState::Destroying || (r.state == ResourceState::Created && r.native_id.as_deref() == Some(f.native_id.as_str())) => {}
            Some(r) => {
                tx.created(r.id, &f.native_id)?;
                recorded.made.push(r.id);
            }
            None => {
                let id = tx.creating(&NewResource::new(run, &f.key, f.kind, by).living(f.lifetime))?;
                tx.created(id, &f.native_id)?;
                recorded.made.push(id);
            }
        }
    }
    for r in &live {
        let still = found
            .iter()
            .any(|f| f.key == r.key && f.kind == r.kind && r.native_id.as_deref().is_none_or(|id| id == f.native_id));
        if !still && r.native_id.is_some() {
            if r.state != ResourceState::Destroying {
                tx.destroying(r.id, by)?;
            }
            tx.gone(r.id)?;
            recorded.gone.push(r.id);
        }
    }
    Ok(recorded)
}

/// The live provider rows of `run` that a removal must still take, by recorded id: those the
/// tool's own removal left (or a dead operation made). A row still `creating` with no id is left
/// out: no id, nothing to remove by (the next [`record_found`] gives it one, or nothing made it).
///
/// # Errors
///
/// The store's.
pub(crate) fn left_to_remove(tx: &Tx<'_>, run: &RunName) -> Result<Vec<ResourceRow>> {
    Ok(tx
        .live_resources(run)?
        .into_iter()
        .filter(|r| r.scope == Scope::Provider && r.native_id.is_some())
        .collect())
}

#[cfg(test)]
mod tests;
