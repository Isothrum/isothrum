//! The runner's state: the home's transactional store ([`Store`], `<home>/state.db`: the runs,
//! the journal of operations, the events, the resources each run made, the host ports reserved,
//! the leases), and one folder per run for what the tools keep (the reference, with what each
//! target keeps in a run's folder: `docs/reference/state.md`):
//!
//! ```text
//! <home>/state.db            the store (SQLite, WAL: with `state.db-wal` and `state.db-shm`)
//! <home>/slots.lock          the OS lock a start holds while it picks its slot (`${run.slot}`)
//! <home>/ssh/                the runner's SSH key, `id_ed25519` and `.pub`, made on first use
//! <home>/terraform-plugins/  the Terraform provider cache every run of this home shares
//! <home>/relay/              on macOS, the wrappers that send a tool through a loopback relay
//! <home>/runs/<run name>/
//!   lock                the OS lock: one operation per run across processes
//!   ssh_config          the run's SSH settings, with its `known_hosts`, for a run reached over
//!                       SSH
//!   guest-<machine>-<random>.in  a guest delivery's values, one file per delivery, only while
//!                       it is delivered
//!   consoles.json       the graphical consoles the runner chose (VMware's VNC: port and
//!                       password per machine); removed by a down
//!   proxmox-console-<random>.curl  a Proxmox API request's credentials, one file per request,
//!                       only while its curl runs (removed however the request ends)
//!   elevated/<token>/   an elevated session's folder (Hyper-V): each request's `env-<n>.json`,
//!                       its variables, until the broker starts the program
//!   …                   what the manifest keeps in `${run.state_dir}`: Compose's pinned ports,
//!                       Vagrant's state, Terraform's state and module copy
//! ```
//!
//! A home starts empty: files another release left in it are neither read nor removed.

#[cfg(any(windows, test))]
mod acl;
mod db;
mod fs;
mod lock;
mod temp_file;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use isoloom_manifest::{Cloud, Driver, Provider, Target};
use serde::{Deserialize, Serialize};

pub use db::{
    CopyName, Ended, EventCursor, EventPage, Lease, LeaseOutcome, Lifetime, MIRROR_FILE, NewResource, NotPerCopy, OperationId, OperationRow, Outcome, Owner,
    PortReservation, Recovered, Reserver, ResourceId, ResourceKind, ResourceRow, ResourceState, RunRow, Scope, Store, Tx,
};
pub(crate) use fs::{create_private, if_there, private_dir, remove_path, write_new_private, write_private};
pub(crate) use temp_file::TempFile;

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
pub(crate) use lock::{RunLocks, SlotsLock, hold};

use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::events::EventLog;
use crate::name::RunName;
use crate::state::{Desired, Op, RunState};
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
    /// The manifest the run was last brought up or updated from, absolute: what a later
    /// operation that has only the record (a sweep's `down`) reopens.
    pub manifest: PathBuf,
    /// Its target.
    pub target: Target,
    /// Its driver.
    pub driver: Driver,
    /// Its cloud, for a cloud target's run: what the reaper picks runs by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<Cloud>,
    /// The Vagrant provider the run uses, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// What the embedder calls the place the run is on (a server, an account).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// Where it stood after its last operation.
    pub state: RunState,
    /// Every param the run was started with: a later operation (a `down`) replays them, so it acts
    /// on what was created, not on what the embedder's settings say now.
    pub params: BTreeMap<String, String>,
    /// The host ports a local VM run forwards from this computer, by `"<machine>/<port>"`: a free
    /// one the runner pinned at the first start, kept so a resume forwards the same port.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vm_host_ports: BTreeMap<String, u16>,
    /// The boot digest (the manifest's `native.boot_digest`) of the Vagrantfile each VM of the run
    /// was last brought up or updated with, by machine name: what an update compares to reload a
    /// running VM whose boot-time settings changed. Empty for a run recorded before the runner
    /// kept them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vm_boot_digests: BTreeMap<String, String>,
    /// When it was last started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// Its slot on its place (`${run.slot}`, 1 to 99), held from its first start to its `down`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<u8>,
    /// The login its SSH goes through first (`user@host`), when the embedder gave one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_jump: Option<String>,
    /// When it stops itself (the manifest's `expiry`), in seconds since the Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// The last operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<LastOp>,
}

impl RunRecord {
    /// The params a later operation replays: those of the run's start, once it was started
    /// (`started_at`); none for a run never started, whose record holds no start's.
    pub(crate) fn replayed_params(&self) -> Option<&BTreeMap<String, String>> {
        self.started_at.is_some().then_some(&self.params)
    }

    /// A record of a run never started yet.
    pub(crate) fn fresh(name: RunName, manifest: PathBuf, target: Target, driver: Driver) -> RunRecord {
        RunRecord {
            version: RECORD_VERSION,
            name,
            manifest,
            target,
            driver,
            cloud: None,
            provider: None,
            host_label: None,
            state: RunState::Absent,
            params: BTreeMap::new(),
            vm_host_ports: BTreeMap::new(),
            vm_boot_digests: BTreeMap::new(),
            started_at: None,
            slot: None,
            ssh_jump: None,
            expires_at: None,
            last: None,
        }
    }
}

/// What the runner last saw of a run when it read it (a status, an operation's own reads): its
/// machines, and where their ports answer. Never an output's value, which may be secret. What a
/// status shows while an operation holds the run, whose tools it then doesn't ask.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LastObserved {
    /// Every machine of the manifest, as read.
    pub machines: Vec<MachineStatus>,
    /// The host port each published machine port got: its machine, its port's name, the port.
    pub published: Vec<(String, String, u16)>,
    /// Where each machine's published ports answer.
    pub hosts: BTreeMap<String, String>,
}

/// The runner's home: its folders, and its store.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    runs: PathBuf,
    /// The home's `slots.lock`: one slot pick at a time.
    slots_lock: PathBuf,
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
        Ok(Home {
            runs,
            slots_lock: home.join("slots.lock"),
            db,
        })
    }

    /// Holds the home's slots while a start picks its slot and records it.
    pub(crate) async fn hold_slots(&self) -> Result<SlotsLock> {
        lock::hold_slots(&self.slots_lock).await
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

    /// Whether an operation holds run `name` now, in this process or another ([`lock::held`]).
    pub(crate) fn held(&self, name: &RunName) -> Result<bool> {
        let path = self.lock_path(name);
        lock::held(&path).map_err(Error::io(&path, "read"))
    }

    /// The home's transactional store.
    pub(crate) fn db(&self) -> &Arc<Store> {
        &self.db
    }

    /// What the runner last saw of run `name`, when it ever read it.
    pub(crate) fn last_observed(&self, name: &RunName) -> Result<Option<LastObserved>> {
        Ok(self.db.read(|tx| tx.run(name))?.and_then(|r| r.observed))
    }

    /// Keeps `seen` as what the runner last saw of run `name`. A run with no row yet gets the
    /// one `fresh` gives, if any (an operation's run); else keeps nothing (a query's).
    pub(crate) fn keep_seen(&self, name: &RunName, seen: &LastObserved, fresh: Option<RunRecord>) -> Result<()> {
        self.db.write(|tx| {
            if tx.keep_observed(name, seen)? {
                return Ok(());
            }
            if let Some(record) = fresh {
                tx.save_record(&record, Desired::Running)?;
                tx.keep_observed(name, seen)?;
            }
            Ok(())
        })
    }

    /// Status = reconcile(desired, observed): keeps `seen`, and moves a settled run to `state`,
    /// what its machines show, when the state machine has such a move.
    pub(crate) fn reconcile(&self, name: &RunName, seen: &LastObserved, state: RunState) -> Result<Option<crate::state::Transition>> {
        self.db.write(|tx| tx.observe(name, seen, state))
    }

    /// Forgets what the runner last saw of run `name` (a run gone: the next start is a new run).
    pub(crate) fn forget_seen(&self, name: &RunName) -> Result<()> {
        self.db.write(|tx| tx.forget_observed(name))
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

    /// Moves run `name` by `transition`.
    pub(crate) fn apply(&self, name: &RunName, transition: crate::state::Transition) -> Result<()> {
        self.db.write(|tx| tx.apply(name, transition))
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

/// The JSON file at `path`, read; `None` when there is none.
///
/// # Errors
///
/// It exists but can't be read (`io`), or doesn't hold a `T` (`corrupt`).
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let Some(bytes) = if_there(std::fs::read(path)).map_err(Error::io(path, "read"))? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes).map(Some).map_err(|source| Error::Corrupt {
        path: path.to_path_buf(),
        what: "JSON",
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str) -> RunRecord {
        RunRecord::fresh(
            RunName::new(name).unwrap(),
            "/work/x/.isoloom/docker/run.json".into(),
            Target::Docker,
            Driver::Compose,
        )
    }

    #[test]
    fn records_are_kept_per_run_and_listed_by_name() {
        let home = tempfile::tempdir().unwrap();
        let store = Home::open(home.path(), None).unwrap();
        assert_eq!(store.record(&RunName::new("b").unwrap()).unwrap(), None);
        let mut b = record("b");
        b.params.insert("ISOLOOM_PUBLISH_FIXED".into(), "1".into());
        store.save(&b).unwrap();
        store.save(&record("a")).unwrap();
        let names: Vec<String> = store.records().unwrap().iter().map(|r| r.name.to_string()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(store.record(&b.name).unwrap(), Some(b));
    }
}
