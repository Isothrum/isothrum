//! The store's mirror (NTFS's `$MFTMirr`): the rows a later operation can't do without, copied
//! to `<home>/state.mirror` after each committed transaction that changed them, so a store whose
//! file is damaged can be rebuilt. It carries every run (but its last observation, which a status
//! read refreshes), every live resource with its recorded id, the operations those reference and
//! every open one, the schema version and the event cursor.
//!
//! The file is written whole and atomically: a temporary file beside it, `fsync`, `rename` over
//! the mirror, then `fsync` of the folder. A crash leaves the previous mirror or the new one,
//! never half of one.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use rusqlite::types::{Value, ValueRef};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::{Error, ErrorCode};

/// The mirror's file, in the home.
pub const MIRROR_FILE: &str = "state.mirror";

/// How far the event cursor jumps at a repair past the mirrored one: events written after the
/// mirror's last write are lost with the damaged file, and no subscriber's cursor may be ahead of
/// the new store's.
const CURSOR_MARGIN: i64 = 1_000_000;

/// The run columns the mirror leaves out: what a status read refreshes.
const NOT_MIRRORED: [&str; 2] = ["observed", "observed_at"];

/// A table's rows, each column by name.
type Rows = Vec<BTreeMap<String, serde_json::Value>>;

/// What the mirror holds.
#[derive(Debug, Serialize, Deserialize)]
struct Mirror {
    /// The schema version of the store it copies.
    schema: i64,
    /// The store's mirror generation when it was written.
    generation: i64,
    /// When.
    written_at: String,
    /// The newest event's cursor then.
    events_cursor: i64,
    /// The runs.
    runs: Rows,
    /// The operations the runs and resources reference, and every open one.
    operations: Rows,
    /// The live resources.
    resources: Rows,
}

/// A repair from the mirror: where the damaged store was kept, when the mirror was written, and
/// what was wrong with the store.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Recovered {
    /// Where the damaged file was moved (its `-wal` and `-shm` beside it, with the same suffix).
    pub corrupt_kept_at: PathBuf,
    /// When the mirror it was rebuilt from was written.
    pub mirror_written_at: String,
    /// What was wrong with the store.
    pub reason: String,
}

/// The mirror generation `conn`'s store is at.
pub(super) fn generation(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT n FROM mirror_generation WHERE id = 1", [], |r| r.get(0))
}

/// `conn`'s rows of `sql`, each column by name (BLOBs aren't in the mirrored tables).
fn rows(conn: &Connection, sql: &str) -> rusqlite::Result<Rows> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().map(str::to_owned).collect();
    let mut out = Vec::new();
    let mut found = stmt.query([])?;
    while let Some(row) = found.next()? {
        let mut columns = BTreeMap::new();
        for (i, name) in names.iter().enumerate() {
            if NOT_MIRRORED.contains(&name.as_str()) {
                continue;
            }
            let value = match row.get_ref(i)? {
                ValueRef::Null | ValueRef::Blob(_) => serde_json::Value::Null,
                ValueRef::Integer(n) => n.into(),
                ValueRef::Real(f) => f.into(),
                ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
            };
            columns.insert(name.clone(), value);
        }
        out.push(columns);
    }
    Ok(out)
}

/// Writes `conn`'s mirror to `path`, at `generation` (read in the same snapshot by the caller).
///
/// # Errors
///
/// The store can't be read, or the file can't be written.
pub(super) fn write(conn: &Connection, generation: i64, path: &Path) -> Result<()> {
    let read = |e: rusqlite::Error| super::failed("read")(e);
    let schema: i64 = conn
        .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))
        .map_err(read)?;
    let events_cursor: i64 = conn.query_row("SELECT COALESCE(MAX(cursor), 0) FROM events", [], |r| r.get(0)).map_err(read)?;
    let mirror = Mirror {
        schema,
        generation,
        written_at: super::now(),
        events_cursor,
        runs: rows(conn, "SELECT * FROM runs ORDER BY name").map_err(read)?,
        operations: rows(
            conn,
            "SELECT * FROM operations WHERE ended_at IS NULL
               OR id IN (SELECT state_op FROM runs)
               OR id IN (SELECT created_by FROM resources WHERE state <> 'gone')
               OR id IN (SELECT destroyed_by FROM resources WHERE state <> 'gone')
             ORDER BY id",
        )
        .map_err(read)?,
        resources: rows(conn, "SELECT * FROM resources WHERE state <> 'gone' ORDER BY id").map_err(read)?,
    };
    let text = serde_json::to_vec(&mirror).map_err(|e| Error::io(path, "written")(e.into()))?;
    write_durably(path, &text).map_err(Error::io(path, "written"))
}

/// Writes `contents` to `path` so that a crash leaves the old file or the new one: a temporary
/// file beside it, flushed to disk, renamed over it, and the folder flushed.
fn write_durably(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("mirror.tmp");
    {
        let mut f = crate::store::create_private(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

/// The mirror at `path`, when there is one this runner reads.
pub(super) fn read(path: &Path) -> Option<MirrorFile> {
    let bytes = std::fs::read(path).ok()?;
    match serde_json::from_slice::<Mirror>(&bytes) {
        Ok(mirror) => Some(MirrorFile(mirror)),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "the store's mirror can't be read");
            None
        }
    }
}

/// A mirror read back.
#[derive(Debug)]
pub(super) struct MirrorFile(Mirror);

impl MirrorFile {
    /// When it was written.
    pub(super) fn written_at(&self) -> &str {
        &self.0.written_at
    }

    /// Whether it holds anything: a run, an operation or a resource.
    pub(super) fn holds_rows(&self) -> bool {
        !(self.0.runs.is_empty() && self.0.operations.is_empty() && self.0.resources.is_empty())
    }

    /// The schema version it copies.
    pub(super) fn schema(&self) -> i64 {
        self.0.schema
    }

    /// Puts its rows into `conn`, a store just made at the same schema, in one transaction.
    ///
    /// # Errors
    ///
    /// A row can't be written (`store_corrupt`: the mirror doesn't fit the schema).
    pub(super) fn restore(&self, conn: &mut Connection) -> Result<()> {
        let corrupt = |e: rusqlite::Error| Error::Store {
            action: "rebuilt from its mirror",
            code: ErrorCode::StoreCorrupt,
            reason: e.to_string(),
        };
        let tx = conn.transaction().map_err(corrupt)?;
        // The rows reference each other (a run its last operation, an operation the one it
        // recovered): checked once all are in.
        tx.pragma_update(None, "defer_foreign_keys", "ON").map_err(corrupt)?;
        for (table, rows) in [("operations", &self.0.operations), ("runs", &self.0.runs), ("resources", &self.0.resources)] {
            for row in rows {
                insert(&tx, table, row).map_err(corrupt)?;
            }
        }
        // An operation one of them recovered, but that the mirror didn't need.
        tx.execute(
            "UPDATE operations SET resumed_from = NULL WHERE resumed_from IS NOT NULL AND resumed_from NOT IN (SELECT id FROM operations)",
            [],
        )
        .map_err(corrupt)?;
        tx.execute(
            "INSERT OR REPLACE INTO sqlite_sequence (name, seq) VALUES ('events', ?1)",
            [self.0.events_cursor.saturating_add(CURSOR_MARGIN)],
        )
        .map_err(corrupt)?;
        tx.commit().map_err(corrupt)
    }
}

/// Inserts `row` into `table`, column by column.
fn insert(tx: &rusqlite::Transaction<'_>, table: &str, row: &BTreeMap<String, serde_json::Value>) -> rusqlite::Result<()> {
    let names: Vec<&str> = row.keys().map(String::as_str).collect();
    let marks: Vec<String> = (1..=names.len()).map(|i| format!("?{i}")).collect();
    let sql = format!("INSERT INTO {table} ({}) VALUES ({})", names.join(", "), marks.join(", "));
    let values: Vec<Value> = row
        .values()
        .map(|v| match v {
            serde_json::Value::Null => Value::Null,
            serde_json::Value::Number(n) => n.as_i64().map_or_else(|| Value::Real(n.as_f64().unwrap_or_default()), Value::Integer),
            serde_json::Value::Bool(b) => Value::Integer(i64::from(*b)),
            serde_json::Value::String(s) => Value::Text(s.clone()),
            other => Value::Text(other.to_string()),
        })
        .collect();
    tx.execute(&sql, rusqlite::params_from_iter(values))?;
    Ok(())
}
