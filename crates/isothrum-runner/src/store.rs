//! The runner's state: the home's transactional store ([`Store`], `<home>/state.db`: the runs,
//! the journal of operations, the events, the resources each run made, what the runs' downloads
//! fetched, the host ports reserved, the leases), and one folder per run for what the tools keep
//! (the reference, with what each target keeps in a run's folder: `docs/reference/state.md`):
//!
//! ```text
//! <home>/state.db            the store (SQLite, WAL: with `state.db-wal` and `state.db-shm`)
//! <home>/ssh/                the runner's SSH key, `id_ed25519` and `.pub`, made on first use
//! <home>/terraform-plugins/  the Terraform provider cache every run of this home shares
//! <home>/runs/<run name>/
//!   lock                the OS lock: one operation per run across processes
//!   elevated/<token>/   an elevated session's folder (Hyper-V): each request's `env-<n>.json`,
//!                       its variables, until the broker starts the program
//!   …                   what each format's driver keeps: Vagrant's dotfile folder, Terraform's
//!                       state
//! ```
//!
//! A home starts empty: files another release left in it are neither read nor removed.

#[cfg(any(windows, test))]
mod acl;
mod db;
mod fs;
mod lock;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[cfg(test)]
pub(crate) use db::dead_pid;
pub use db::{
    CopyName, Ended, EventCursor, EventPage, Lease, LeaseOutcome, Lifetime, MIRROR_FILE, NewResource, NotPerCopy, OperationId, OperationRow, Outcome, Owner,
    PortReservation, Recovered, Reserver, ResourceId, ResourceKind, ResourceRow, ResourceState, RunRow, Scope, Store, Tx,
};
pub(crate) use fs::{create_private, if_there, private_dir, remove_path, write_private};

/// Creates `dir` (with its missing parents) owner-only ([`private_dir`]); [`Error::Io`] naming it
/// when that fails.
pub(crate) fn create_dir(dir: &Path) -> Result<()> {
    private_dir(dir).map_err(Error::io(dir, "created"))
}

/// Writes `contents` to `path` whole and owner-only ([`write_private`]); [`Error::Io`] naming it
/// when that fails.
pub(crate) fn write(path: &Path, contents: &[u8]) -> Result<()> {
    write_private(path, contents).map_err(Error::io(path, "written"))
}

/// Removes `path`, a file or a folder, nothing there being fine ([`remove_path`]); [`Error::Io`]
/// naming it when that fails.
pub(crate) fn remove(path: &Path) -> Result<()> {
    remove_path(path).map_err(Error::io(path, "removed"))
}

/// The JSON a runner wrote at `path` ([`write_json`]), read back; `T`'s default when nothing is
/// there. [`Error::Io`] naming it when it can't be read or isn't what was written.
pub(crate) fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> Result<T> {
    match if_there(std::fs::read_to_string(path)).map_err(Error::io(path, "read"))? {
        Some(text) => serde_json::from_str(&text).map_err(|e| Error::io(path, "read")(std::io::Error::new(std::io::ErrorKind::InvalidData, e))),
        None => Ok(T::default()),
    }
}

/// `value` as JSON at `path`, its folder made first ([`create_dir`], [`write`]).
pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        create_dir(dir)?;
    }
    let json = serde_json::to_string(value).map_err(|e| Error::io(path, "written")(std::io::Error::other(e)))?;
    write(path, json.as_bytes())
}
pub(crate) use lock::{RunLocks, hold};

use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::events::EventLog;
use crate::name::RunName;
use crate::project::Format;
use crate::project::platform::{Cloud, Provider};
use crate::state::{Desired, Op, Park, RunState};
use crate::status::MachineStatus;

/// The record format's version.
const RECORD_VERSION: u32 = 0;

/// A run's OS lock file, in its folder.
const LOCK_FILE: &str = "lock";

/// How many event lines a run's log keeps before an operation trims it to half.
pub(crate) const EVENTS_KEPT: usize = 4000;

/// The last operation on a run and how it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LastOp {
    /// The operation.
    pub op: Op,
    /// Whether it succeeded.
    pub ok: bool,
    /// Its failure's code, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
    /// When it ended.
    pub at: String,
}

impl LastOp {
    /// Operation `op`, ended at `at` with failure `code`, or succeeded without one: the one place
    /// `ok` is derived, so it never disagrees with `code`.
    pub(crate) fn ended(op: Op, code: Option<ErrorCode>, at: String) -> LastOp {
        LastOp {
            op,
            ok: code.is_none(),
            code,
            at,
        }
    }
}

/// What the runner keeps about a run (never a secret's value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunRecord {
    /// The record format's version.
    pub version: u32,
    /// The run's name.
    pub name: RunName,
    /// The project folder the run was last brought up or updated from, absolute: what a later
    /// operation that has only the record (a sweep's `down`) reopens.
    #[serde(alias = "manifest")]
    pub project: PathBuf,
    /// The project's format.
    #[serde(alias = "driver")]
    pub format: Format,
    /// Its cloud, for a run on one: what the embedder sweeps runs by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<Cloud>,
    /// The Vagrant provider the run uses, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// The Docker context its `docker` calls go to ([`crate::RunOptions::docker_context`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docker_context: Option<String>,
    /// What the embedder calls the place the run is on (a server, an account).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// Where it stood after its last operation.
    pub state: RunState,
    /// How its last park parked it ([`Park::Pause`] or [`Park::Shutdown`]); none once a start,
    /// a resume or a down followed. Meaningful while `state` is [`RunState::Parked`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked: Option<Park>,
    /// When it was last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When it may be removed ([`crate::RunOptions::lifetime`] from its start), in seconds since
    /// the Unix epoch: what the reaper reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// The last operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<LastOp>,
    /// The values (not the secrets, which are never kept) the run was started or last updated
    /// with ([`crate::RunOptions::with_env`]): every later operation gives them to the tool again
    /// unless the caller gives a value of the same name, so a `down`, an auto-stop or an update
    /// sees the variables its start applied (a Terraform module's `region`).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
}

impl RunRecord {
    /// Whether its lifetime ([`crate::RunOptions::lifetime`]) passed at `now` and its last
    /// operation didn't leave it absent: a run that may still hold something, for a reaper to
    /// take down.
    #[must_use]
    pub fn expired(&self, now: std::time::SystemTime) -> bool {
        let now = crate::clock::unix_seconds(now);
        self.state != RunState::Absent && self.expires_at.is_some_and(|at| at <= now)
    }

    /// A record of a run of the `format` project in `project`, never started yet.
    pub(crate) fn fresh(name: RunName, project: PathBuf, format: Format) -> RunRecord {
        RunRecord {
            version: RECORD_VERSION,
            name,
            project,
            format,
            cloud: None,
            provider: None,
            docker_context: None,
            host_label: None,
            state: RunState::Absent,
            parked: None,
            started_at: None,
            expires_at: None,
            last: None,
            env: std::collections::BTreeMap::new(),
        }
    }
}

/// What the runner last saw of a run when it read it (a status, an operation's own reads): its
/// machines, and where their ports answer. Never an output's value, which may be secret. What a
/// status shows while an operation holds the run, whose tools it then doesn't ask.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LastObserved {
    /// Every machine the tool reported, as read.
    pub machines: Vec<MachineStatus>,
}

/// The runner's home: its folders, and its store.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    runs: PathBuf,
    /// The home's store: the runs, the journal, the events, the resources, the leases.
    db: Arc<Store>,
}

impl Home {
    /// The home's folders under `home` (owner-only on Windows, see [`fs::private_home`]), its `runs/`
    /// folder created owner-only.
    pub(crate) fn open(home: &Path, profile: Option<&Path>) -> Result<Home> {
        fs::private_home(home, profile).map_err(Error::io(home, "made owner-only"))?;
        let runs = home.join("runs");
        create_dir(&runs)?;
        let db = Arc::new(Store::open(home)?);
        Ok(Home { runs, db })
    }

    /// Run `name`'s folder, created owner-only.
    pub(crate) fn run_dir(&self, name: &RunName) -> Result<PathBuf> {
        let dir = self.run_dir_path(name);
        create_dir(&dir)?;
        Ok(dir)
    }

    /// Run `name`'s folder (not made: [`Store::run_dir`] makes it).
    pub(crate) fn run_dir_path(&self, name: &RunName) -> PathBuf {
        self.runs.join(name.as_str())
    }

    /// Run `name`'s lock file.
    pub(crate) fn lock_path(&self, name: &RunName) -> PathBuf {
        self.run_dir_path(name).join(LOCK_FILE)
    }

    /// The home's transactional store.
    pub(crate) fn db(&self) -> &Arc<Store> {
        &self.db
    }

    /// Run `name`'s record, when it has one.
    pub(crate) fn record(&self, name: &RunName) -> Result<Option<RunRecord>> {
        Ok(self.db.read(|tx| tx.run(name))?.map(|r| r.record))
    }

    /// Records `record`'s fields; a new run's row starts in the record's state. An existing run's
    /// state moves only by a transition ([`Home::apply`]).
    pub(crate) fn save(&self, record: &RunRecord) -> Result<()> {
        self.run_dir(&record.name)?;
        self.db.write(|tx| tx.save_record(record, Desired::Running))
    }

    /// Records `record`, an adopted deployment's, as a run that is up: a new row is made ready,
    /// one an earlier `down` left absent moves to ready ([`crate::state::Transition::adopted`]).
    pub(crate) fn save_adopted(&self, record: &RunRecord) -> Result<()> {
        self.run_dir(&record.name)?;
        self.db.write(|tx| {
            tx.save_record(record, Desired::Running)?;
            if let Some(row) = tx.run(&record.name)?
                && row.state != RunState::Ready
            {
                tx.apply(&record.name, crate::state::Transition::adopted(row.state)?)?;
            }
            Ok(())
        })
    }

    /// Every run's record, by name.
    pub(crate) fn records(&self) -> Result<Vec<RunRecord>> {
        self.db.read(|tx| tx.runs().map(|rows| rows.into_iter().map(|r| r.record).collect()))
    }

    /// Trims run `name`'s events to the newer half once they are many.
    pub(crate) fn trim_event_log(&self, name: &RunName) -> Result<()> {
        self.db.write(|tx| {
            let (events, unreadable) = tx.events_after(EventCursor::default(), Some(name), EVENTS_KEPT + 1)?;
            if events.len() + unreadable > EVENTS_KEPT {
                tx.trim_events(name, EVENTS_KEPT / 2)?;
            }
            Ok(())
        })
    }

    /// Run `name`'s logged events, oldest first, with how many of its rows aren't an event this
    /// runner reads (another version's kind).
    pub(crate) fn events(&self, name: &RunName) -> Result<EventLog> {
        let (events, unreadable) = self.db.read(|tx| tx.events_after(EventCursor::default(), Some(name), usize::MAX))?;
        Ok(EventLog {
            events: events.into_iter().map(|(_, e)| e).collect(),
            unreadable,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str) -> RunRecord {
        RunRecord::fresh(RunName::new(name).unwrap(), "/work/x".into(), Format::Compose)
    }

    #[test]
    fn records_are_kept_per_run_and_listed_by_name() {
        let home = tempfile::tempdir().unwrap();
        let store = Home::open(home.path(), None).unwrap();
        assert_eq!(store.record(&RunName::new("b").unwrap()).unwrap(), None);
        let mut b = record("b");
        b.host_label = Some("server".into());
        store.save(&b).unwrap();
        store.save(&record("a")).unwrap();
        let names: Vec<String> = store.records().unwrap().iter().map(|r| r.name.to_string()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(store.record(&b.name).unwrap(), Some(b));
    }
}
