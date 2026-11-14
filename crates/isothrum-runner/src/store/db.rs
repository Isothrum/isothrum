//! The home's transactional store: one SQLite database, `<home>/state.db`, in WAL mode with
//! foreign keys on. It holds the runs (what the user wants of each and where each stands), the
//! journal of operations, the events, the resources each run made (written ahead of the call
//! that makes them), the host ports reserved, and the leases. Every change is one transaction.
//!
//! The store is synchronous: its transactions are a few rows, and the runner calls it inline
//! from async code, as it read and wrote its files before.

mod journal;
mod leases;
mod mirror;
mod owner;
mod resources;
mod rows;
mod runs;
mod schema;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub use leases::Reserver;
pub use mirror::{MIRROR_FILE, Recovered};
pub use owner::Owner;
pub use rows::{
    CopyName, Ended, EventCursor, EventPage, Lease, LeaseOutcome, Lifetime, NewResource, NotPerCopy, OperationId, OperationRow, Outcome, PortReservation,
    ResourceId, ResourceKind, ResourceRow, ResourceState, RunRow, Scope,
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
    /// The mirror's file ([`MIRROR_FILE`]).
    mirror: PathBuf,
    /// The mirror generation the mirror was last written at (-1: not since this open).
    mirrored: AtomicI64,
    /// The repair `open` made from the mirror, if it made one.
    recovered: Option<Recovered>,
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
    /// The home's store, `<home>/state.db`: created owner-only when missing, checked
    /// (`PRAGMA quick_check`) and migrated to this runner's schema. A store whose file is damaged
    /// is rebuilt from its mirror (`<home>/state.mirror`, [`MIRROR_FILE`]): the damaged file is
    /// kept aside for diagnostics (`state.db.corrupt-<unix seconds>`), and [`Store::recovered`]
    /// says so. Nothing else of the home is read: files another release left in it are ignored.
    ///
    /// # Errors
    ///
    /// `io` when the home or the file can't be made; `store_corrupt` when the file isn't a sound
    /// database and no mirror can rebuild it; `store_version` when a newer runner wrote it;
    /// `locked` when another process's write held it past five seconds.
    pub fn open(home: &Path) -> Result<Store> {
        let home = &std::path::absolute(home).map_err(Error::io(home, "made absolute"))?;
        crate::store::create_dir(home)?;
        let path = home.join(DB_FILE);
        let mirror = home.join(MIRROR_FILE);
        match Store::open_checked(&path, &mirror, Fresh::Suspect) {
            Ok(store) => {
                // A sound store's mirror is made current at once: one a writer left ahead of its
                // commit (killed in between) is replaced.
                store.sync_mirror();
                Ok(store)
            }
            Err(e) if e.code() == ErrorCode::StoreCorrupt => Store::rebuild(&path, &mirror, &e),
            Err(e) => Err(e),
        }
    }

    /// The store at `path`, opened, checked and migrated; a new, empty file beside a mirror that
    /// holds something is damaged, unless `fresh` says it is the rebuild's own.
    fn open_checked(path: &Path, mirror: &Path, fresh: Fresh) -> Result<Store> {
        let conn = connect(path)?;
        quick_check(&conn)?;
        let store = Store {
            conn: Mutex::new(conn),
            path: path.to_path_buf(),
            mirror: mirror.to_path_buf(),
            mirrored: AtomicI64::new(-1),
            recovered: None,
        };
        let was = store.migrate()?;
        // An empty file where a store was (truncated, replaced) beside a mirror that holds
        // something: damaged, not new.
        if was == 0 && fresh == Fresh::Suspect && mirror::read(mirror).is_some_and(|m| m.holds_rows()) {
            return Err(Error::Store {
                action: "opened",
                code: ErrorCode::StoreCorrupt,
                reason: "its file is empty, but its mirror holds runs, operations or resources".to_owned(),
            });
        }
        Ok(store)
    }

    /// Rebuilds the damaged store at `path` (what was wrong: `cause`) from the mirror at
    /// `mirror`: the damaged files move aside, a new store is made and filled from the mirror.
    fn rebuild(path: &Path, mirror: &Path, cause: &Error) -> Result<Store> {
        let damaged = |reason: String| Error::Store {
            action: "opened",
            code: ErrorCode::StoreCorrupt,
            reason,
        };
        let Some(copy) = mirror::read(mirror) else {
            return Err(damaged(format!("{cause}; and there is no mirror to rebuild it from")));
        };
        let known = i64::try_from(schema::MIGRATIONS.len()).unwrap_or(i64::MAX);
        if copy.schema() != known {
            return Err(damaged(format!("{cause}; and its mirror is of schema {}, not {known}", copy.schema())));
        }
        let kept = set_aside(path)?;
        tracing::warn!(error = %cause, kept = %kept.display(), "the store was damaged: rebuilt from its mirror");
        let mut store = Store::open_checked(path, mirror, Fresh::Expected)?;
        {
            let mut conn = store.conn.lock().unwrap_or_else(PoisonError::into_inner);
            copy.restore(&mut conn)?;
        }
        store.sync_mirror();
        store.recovered = Some(Recovered {
            corrupt_kept_at: kept,
            mirror_written_at: copy.written_at().to_owned(),
            reason: cause.to_string(),
        });
        Ok(store)
    }

    /// The repair [`Store::open`] made from the mirror, if it made one: an embedder then reads
    /// every run's status, which reconciles it with what its tools report.
    #[must_use]
    pub fn recovered(&self) -> Option<&Recovered> {
        self.recovered.as_ref()
    }

    /// Writes the mirror when the store changed since it was last written (its own snapshot).
    fn sync_mirror(&self) {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        match conn.transaction_with_behavior(TransactionBehavior::Deferred) {
            Ok(tx) => self.mirror_within(&tx),
            Err(e) => tracing::warn!(error = %e, "the store's mirror wasn't written"),
        }
    }

    /// Writes the mirror from `conn`'s view when the mirror generation moved since it was last
    /// written. Inside a write transaction, the store's write lock orders the mirrors of every
    /// process. A mirror that can't be written is said, not failed on: the store holds the rows.
    fn mirror_within(&self, conn: &Connection) {
        let written = mirror::generation(conn).map_err(failed("read")).and_then(|n| {
            if n == self.mirrored.load(Ordering::SeqCst) {
                Ok(None)
            } else {
                mirror::write(conn, n, &self.mirror).map(|()| Some(n))
            }
        });
        match written {
            Ok(Some(n)) => self.mirrored.store(n, Ordering::SeqCst),
            Ok(None) => {}
            Err(e) => {
                self.mirrored.store(-1, Ordering::SeqCst);
                tracing::warn!(error = %e, "the store's mirror wasn't written");
            }
        }
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
        let committed = {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate).map_err(failed("written"))?;
            let mut tx = Tx { tx };
            let value = f(&mut tx)?;
            // Before the commit, under the write lock: a mirror left ahead by a crash right after
            // is rewritten at the next open of the sound store.
            self.mirror_within(&tx.tx);
            tx.tx.commit().map(|()| value)
        };
        committed.map_err(|e| {
            // The mirror was written ahead of a commit that failed: written again from what is.
            self.mirrored.store(-1, Ordering::SeqCst);
            drop(conn);
            self.sync_mirror();
            failed("written")(e)
        })
    }

    /// Runs the migrations the file hasn't had, each in its own transaction: the version it was
    /// at (0: a new, empty file).
    fn migrate(&self) -> Result<i64> {
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
        Ok(applied)
    }
}

impl Tx<'_> {}

/// What a new, empty store file means when [`Store::open`] finds one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fresh {
    /// A home's first store, or one emptied: damaged when its mirror holds something.
    Suspect,
    /// The file a rebuild made.
    Expected,
}

/// A connection to the store's file at `path`: owner-only, WAL, foreign keys on.
fn connect(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(failed("opened"))?;
    #[cfg(unix)]
    restrict(path)?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(failed("opened"))?;
    conn.pragma_update(None, "journal_mode", "WAL").map_err(failed("opened"))?;
    conn.pragma_update(None, "foreign_keys", "ON").map_err(failed("opened"))?;
    conn.pragma_update(None, "synchronous", "NORMAL").map_err(failed("opened"))?;
    Ok(conn)
}

/// How many of `quick_check`'s findings an error names.
const FINDINGS_SAID: usize = 3;

/// Checks the store's pages (`PRAGMA quick_check`, as `chkdsk` checks a volume at mount).
///
/// # Errors
///
/// `store_corrupt`, naming its first findings, when the check finds a problem.
fn quick_check(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA quick_check").map_err(failed("checked"))?;
    let findings: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(failed("checked"))?
        .collect::<rusqlite::Result<_>>()
        .map_err(failed("checked"))?;
    if findings.iter().all(|f| f == "ok") {
        return Ok(());
    }
    let said: Vec<&str> = findings.iter().take(FINDINGS_SAID).map(String::as_str).collect();
    Err(Error::Store {
        action: "checked",
        code: ErrorCode::StoreCorrupt,
        reason: format!("its integrity check failed ({} findings): {}", findings.len(), said.join("; ")),
    })
}

/// Moves the damaged store at `path` (and its `-wal` and `-shm`) aside, to
/// `state.db.corrupt-<unix seconds>`: where it is now.
fn set_aside(path: &Path) -> Result<PathBuf> {
    let kept = path.with_file_name(format!("{DB_FILE}.corrupt-{}", epoch_now()));
    for suffix in ["", "-wal", "-shm"] {
        let with = |p: &Path| {
            let mut name = p.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name)
        };
        let (from, to) = (with(path), with(&kept));
        if from.exists() {
            std::fs::rename(&from, &to).map_err(Error::io(&from, "moved aside"))?;
        }
    }
    Ok(kept)
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
