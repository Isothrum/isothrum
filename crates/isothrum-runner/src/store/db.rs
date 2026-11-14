//! The home's transactional store: one SQLite database, `<home>/state.db`, in WAL mode with
//! foreign keys on. It holds the runs (what the user wants of each and where each stands), the
//! journal of operations, the events, the resources each run made (written ahead of the call
//! that makes them), the host ports reserved, and the leases. Every change is one transaction.
//!
//! The store is synchronous: its transactions are a few rows, and the runner calls it inline
//! from async code, as it read and wrote its files before.

mod import;
mod journal;
mod leases;
mod owner;
mod resources;
mod rows;
mod runs;
mod schema;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use leases::Reserver;
pub use owner::Owner;
pub use rows::{
    CopyName, Ended, EventCursor, Lease, LeaseOutcome, Lifetime, NewResource, NotPerCopy, OperationId, OperationRow, Outcome, PortReservation, ResourceId,
    ResourceKind, ResourceRow, ResourceState, RunRow, Scope,
};

use crate::Result;
use crate::error::{Error, ErrorCode};

/// The store's file, in the home.
pub(crate) const DB_FILE: &str = "state.db";

/// How long a write waits for another process's write before it fails `locked`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The home's store.
pub struct Store {
    conn: Mutex<Connection>,
    path: PathBuf,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store").field("path", &self.path).finish_non_exhaustive()
    }
}

/// A transaction over the store: every query and change is one of its methods; the changes need
/// `&mut`, so they are made only in [`Store::write`].
pub struct Tx<'c> {
    tx: rusqlite::Transaction<'c>,
}

impl Store {
    /// The home's store, `<home>/state.db`: created owner-only when missing, migrated to this
    /// runner's schema, and, the first time a home a 2.x runner wrote is opened, imported from
    /// its files (`runs/<name>/record.json`, `seen.json`, `events.jsonl`, the
    /// anonymous volumes), which are then left read-only beside it.
    ///
    /// # Errors
    ///
    /// `io` when the home or the file can't be made; `store_corrupt` when the file isn't a
    /// database; `store_version` when a newer runner wrote it; `locked` when another process's
    /// write held it past five seconds.
    pub fn open(home: &Path) -> Result<Store> {
        let home = &std::path::absolute(home).map_err(Error::io(home, "made absolute"))?;
        crate::store::create_dir(home)?;
        let path = home.join(DB_FILE);
        let conn = Connection::open(&path).map_err(failed("opened"))?;
        #[cfg(unix)]
        restrict(&path)?;
        conn.busy_timeout(BUSY_TIMEOUT).map_err(failed("opened"))?;
        conn.pragma_update(None, "journal_mode", "WAL").map_err(failed("opened"))?;
        conn.pragma_update(None, "foreign_keys", "ON").map_err(failed("opened"))?;
        conn.pragma_update(None, "synchronous", "NORMAL").map_err(failed("opened"))?;
        let store = Store { conn: Mutex::new(conn), path };
        store.migrate()?;
        store.write(|tx| import::from_files(tx, home))?;
        Ok(store)
    }

    /// The database's file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Runs `f` in a read-only transaction: a snapshot that sees no write half made.
    ///
    /// # Errors
    ///
    /// `f`'s, or the store's.
    pub fn read<T>(&self, f: impl FnOnce(&Tx<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred).map_err(failed("read"))?;
        let tx = Tx { tx };
        let value = f(&tx)?;
        tx.tx.finish().map_err(failed("read"))?;
        Ok(value)
    }

    /// Runs `f` in a write transaction: it commits when `f` returns `Ok`, and rolls back when `f`
    /// fails (or panics), so every change `f` made is kept or none is.
    ///
    /// # Errors
    ///
    /// `f`'s, or the store's (`locked` when another process's write held it too long).
    pub fn write<T>(&self, f: impl FnOnce(&mut Tx<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(failed("written"))?;
        let mut tx = Tx { tx };
        let value = f(&mut tx)?;
        tx.tx.commit().map_err(failed("written"))?;
        Ok(value)
    }

    /// Runs the migrations the file hasn't had, each in its own transaction.
    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        conn.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL)")
            .map_err(failed("migrated"))?;
        let applied: i64 = conn
            .query_row("SELECT COALESCE(MAX(version), 0) FROM schema_migrations", [], |r| r.get(0))
            .map_err(failed("migrated"))?;
        let known = i64::try_from(schema::MIGRATIONS.len()).unwrap_or(i64::MAX);
        if applied > known {
            return Err(Error::Store {
                action: "opened",
                code: ErrorCode::StoreVersion,
                reason: format!("its schema is version {applied}, newer than this runner's {known}: a newer runner wrote it"),
            });
        }
        for (version, sql) in (1..).zip(schema::MIGRATIONS).skip(usize::try_from(applied).unwrap_or(0)) {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(failed("migrated"))?;
            tx.execute_batch(sql).map_err(failed("migrated"))?;
            tx.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
                rusqlite::params![version, now()],
            )
            .map_err(failed("migrated"))?;
            tx.commit().map_err(failed("migrated"))?;
        }
        Ok(())
    }
}

impl Tx<'_> {
    /// The value of `store_meta`'s `key`.
    pub(crate) fn meta(&self, key: &str) -> Result<Option<String>> {
        self.tx
            .query_row("SELECT value FROM store_meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .map_err(failed("read"))
    }

    /// Sets `store_meta`'s `key`.
    pub(crate) fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO store_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                [key, value],
            )
            .map_err(failed("written"))?;
        Ok(())
    }
}

/// The store's failure to do `action`, with the code SQLite's error has: a database busy past the
/// wait is `locked`, a file that isn't one `store_corrupt`, anything else `io`.
pub(crate) fn failed(action: &'static str) -> impl Fn(rusqlite::Error) -> Error {
    move |e| {
        let code = match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => ErrorCode::Locked,
            Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => ErrorCode::StoreCorrupt,
            _ => ErrorCode::Io,
        };
        Error::Store {
            action,
            code,
            reason: e.to_string(),
        }
    }
}

/// A column that doesn't hold what this runner writes there.
pub(crate) fn corrupt(what: &str, value: &str) -> Error {
    Error::Store {
        action: "read",
        code: ErrorCode::StoreCorrupt,
        reason: format!("a {what} column holds `{value}`, which this runner doesn't write"),
    }
}

/// `value`'s wire name, as a column holds it.
pub(crate) fn word<T: Serialize>(value: &T) -> String {
    crate::encoding::wire_name(value)
}

/// The unit enum `T` a column wrote as `word`.
pub(crate) fn from_word<T: DeserializeOwned>(what: &str, word: &str) -> Result<T> {
    crate::encoding::from_wire_name(word).ok_or_else(|| corrupt(what, word))
}

/// `value` as a JSON column.
pub(crate) fn to_json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| Error::Store {
        action: "written",
        code: ErrorCode::Io,
        reason: e.to_string(),
    })
}

/// A JSON column read back.
pub(crate) fn from_json<T: DeserializeOwned>(what: &str, text: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| Error::Store {
        action: "read",
        code: ErrorCode::StoreCorrupt,
        reason: format!("its {what} doesn't parse: {e}"),
    })
}

/// Now, as the store writes times.
pub(crate) fn now() -> String {
    crate::clock::rfc3339(std::time::SystemTime::now())
}

/// Now, in seconds since the Unix epoch.
pub(crate) fn epoch_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Makes the database's file owner-only (Unix: on Windows the home's DACL already is).
#[cfg(unix)]
fn restrict(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(Error::io(path, "made owner-only"))
}
