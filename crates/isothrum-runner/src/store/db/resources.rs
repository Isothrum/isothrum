//! The resources each run made, written ahead: `creating` before the call that makes one,
//! `created` with the provider's id after it, `destroying` before the call that removes it,
//! `gone` after. A process that dies between the two writes leaves a row that says what may
//! exist, and the next operation removes it by its id, or by its per-copy name.

use rusqlite::{Row, params};

use super::{CopyName, NewResource, OperationId, ResourceId, ResourceRow, ResourceState, Tx, failed, from_json, from_word, now, to_json, word};
use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::name::RunName;

/// The columns a [`ResourceRow`] is read from, in [`resource_row`]'s order.
const RESOURCE_COLUMNS: &str = "id, run, key, kind, scope, copy_name, native_id, state, lifetime, created_by, destroyed_by, detail";

/// The text columns of a resource's row, before they are parsed.
struct RawResource {
    id: i64,
    run: String,
    key: String,
    kind: String,
    scope: String,
    copy_name: Option<String>,
    native_id: Option<String>,
    state: String,
    lifetime: String,
    created_by: i64,
    destroyed_by: Option<i64>,
    detail: Option<String>,
}

fn raw_resource(r: &Row<'_>) -> rusqlite::Result<RawResource> {
    Ok(RawResource {
        id: r.get(0)?,
        run: r.get(1)?,
        key: r.get(2)?,
        kind: r.get(3)?,
        scope: r.get(4)?,
        copy_name: r.get(5)?,
        native_id: r.get(6)?,
        state: r.get(7)?,
        lifetime: r.get(8)?,
        created_by: r.get(9)?,
        destroyed_by: r.get(10)?,
        detail: r.get(11)?,
    })
}

fn resource_row(raw: RawResource) -> Result<ResourceRow> {
    Ok(ResourceRow {
        id: ResourceId(raw.id),
        run: RunName::new(&raw.run).map_err(|_| super::corrupt("run name", &raw.run))?,
        key: raw.key,
        kind: from_word("kind", &raw.kind)?,
        scope: from_word("scope", &raw.scope)?,
        copy_name: raw.copy_name.map(CopyName::stored),
        native_id: raw.native_id,
        state: from_word("resource state", &raw.state)?,
        lifetime: from_word("lifetime", &raw.lifetime)?,
        created_by: OperationId(raw.created_by),
        destroyed_by: raw.destroyed_by.map(OperationId),
        detail: raw.detail.as_deref().map(|d| from_json("detail", d)).transpose()?,
    })
}

impl Tx<'_> {
    /// Writes `new` as `creating`, before the call that makes it: commit it before the call.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when `new` gives a local kind a lifetime other than its own, or a
    /// provider kind none; the store's.
    pub fn creating(&mut self, new: &NewResource<'_>) -> Result<ResourceId> {
        let lifetime = match (new.kind.lifetime(), new.lifetime) {
            (Some(own), None) => own,
            (Some(own), Some(asked)) if own == asked => own,
            (None, Some(asked)) => asked,
            (own, asked) => {
                return Err(Error::Store {
                    action: "written",
                    code: ErrorCode::IllegalTransition,
                    reason: format!(
                        "a {} resource lives {}, not {}",
                        word(&new.kind),
                        own.map_or_else(|| "as its manifest says".to_owned(), |l| word(&l)),
                        asked.map_or_else(|| "unsaid".to_owned(), |l| word(&l)),
                    ),
                });
            }
        };
        self.tx
            .execute(
                "INSERT INTO resources (run, key, kind, scope, copy_name, state, lifetime, created_by, detail, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'creating', ?6, ?7, ?8, ?9)",
                params![
                    new.run.as_str(),
                    new.key,
                    word(&new.kind),
                    word(&new.kind.scope()),
                    new.copy_name.as_ref().map(CopyName::as_str),
                    word(&lifetime),
                    new.by.0,
                    new.detail.as_ref().map(to_json).transpose()?,
                    now()
                ],
            )
            .map_err(failed("written"))?;
        Ok(ResourceId(self.tx.last_insert_rowid()))
    }

    /// Records that resource `id` was made, as `native_id`.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when the row isn't `creating` or `created` (a made resource made
    /// again keeps its row: an idempotent retry); the store's.
    pub fn created(&mut self, id: ResourceId, native_id: &str) -> Result<()> {
        self.move_resource(id, &[ResourceState::Creating, ResourceState::Created], ResourceState::Created, |tx| {
            tx.tx
                .execute("UPDATE resources SET native_id = ?2 WHERE id = ?1", params![id.0, native_id])
                .map(drop)
        })
    }

    /// Records that operation `by` is removing resource `id`: before the call that removes it.
    ///
    /// # Errors
    ///
    /// `illegal_transition` when the row is gone already; the store's.
    pub fn destroying(&mut self, id: ResourceId, by: OperationId) -> Result<()> {
        let from = [ResourceState::Creating, ResourceState::Created, ResourceState::Destroying];
        self.move_resource(id, &from, ResourceState::Destroying, |tx| {
            tx.tx
                .execute("UPDATE resources SET destroyed_by = ?2 WHERE id = ?1", params![id.0, by.0])
                .map(drop)
        })
    }

    /// Records that resource `id` is gone: removed, or found missing. Gone twice is fine.
    ///
    /// # Errors
    ///
    /// The store's.
    pub fn gone(&mut self, id: ResourceId) -> Result<()> {
        let from = [ResourceState::Creating, ResourceState::Created, ResourceState::Destroying, ResourceState::Gone];
        self.move_resource(id, &from, ResourceState::Gone, |_| Ok(()))
    }

    /// Moves resource `id` to `to` when it stands in one of `from`, with `also` in the same
    /// transaction.
    fn move_resource(&mut self, id: ResourceId, from: &[ResourceState], to: ResourceState, also: impl FnOnce(&Tx<'_>) -> rusqlite::Result<()>) -> Result<()> {
        let state: String = self
            .tx
            .query_row("SELECT state FROM resources WHERE id = ?1", [id.0], |r| r.get(0))
            .map_err(failed("read"))?;
        let state: ResourceState = from_word("resource state", &state)?;
        if !from.contains(&state) {
            return Err(Error::Store {
                action: "written",
                code: ErrorCode::IllegalTransition,
                reason: format!("resource {} is {}, so it can't become {}", id.0, word(&state), word(&to)),
            });
        }
        self.tx
            .execute(
                "UPDATE resources SET state = ?2, updated_at = ?3 WHERE id = ?1",
                params![id.0, word(&to), now()],
            )
            .map_err(failed("written"))?;
        also(self).map_err(failed("written"))
    }

    /// Run `run`'s resources not gone, oldest first (a `down` walks them newest first).
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn live_resources(&self, run: &RunName) -> Result<Vec<ResourceRow>> {
        let sql = format!("SELECT {RESOURCE_COLUMNS} FROM resources WHERE run = ?1 AND state <> 'gone' ORDER BY id");
        self.resources_where(&sql, params![run.as_str()])
    }

    /// The resources operation `by` made that aren't gone: what a recovery of it removes.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn resources_of(&self, by: OperationId) -> Result<Vec<ResourceRow>> {
        let sql = format!("SELECT {RESOURCE_COLUMNS} FROM resources WHERE created_by = ?1 AND state <> 'gone' ORDER BY id");
        self.resources_where(&sql, params![by.0])
    }

    /// Run `run`'s live resource keyed `key` of the per-copy name `name`: what an idempotent
    /// create finds before making it again. Never by a bare name: [`CopyName`] holds the key.
    ///
    /// # Errors
    ///
    /// The store can't be read.
    pub fn resource_named(&self, run: &RunName, key: &str, name: &CopyName) -> Result<Option<ResourceRow>> {
        let sql =
            format!("SELECT {RESOURCE_COLUMNS} FROM resources WHERE run = ?1 AND key = ?2 AND copy_name = ?3 AND state <> 'gone' ORDER BY id DESC LIMIT 1");
        Ok(self.resources_where(&sql, params![run.as_str(), key, name.as_str()])?.pop())
    }

    fn resources_where(&self, sql: &str, params: impl rusqlite::Params) -> Result<Vec<ResourceRow>> {
        let mut stmt = self.tx.prepare(sql).map_err(failed("read"))?;
        let found = stmt.query_map(params, raw_resource).map_err(failed("read"))?;
        let mut rows = Vec::new();
        for raw in found {
            rows.push(resource_row(raw.map_err(failed("read"))?)?);
        }
        Ok(rows)
    }
}
