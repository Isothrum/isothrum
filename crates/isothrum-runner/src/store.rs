//! The runner's state, one folder per run under the embedder's home:
//!
//! ```text
//! <home>/runs/<run name>/
//!   record.json     the run's record (atomic, owner-only)
//!   lock            the OS lock: one operation per run across processes
//!   events.jsonl    the last operations' events, so a reattached UI can replay them
//!   …               what the manifest keeps in `${run.state_dir}` (Compose's pinned ports)
//! ```
//!
//! The run's folder holds only the run and its generated files, so replacing it never strands a
//! run. `Store` is a struct over the file system, not a trait: tests use a temporary home.

mod fs;
mod lock;

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use isoloom_manifest::{Cloud, Driver, Target};
use serde::{Deserialize, Serialize};

pub(crate) use fs::{create_private, private_dir, write_private};
pub(crate) use lock::RunLocks;

use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::events::Event;
use crate::name::RunName;
use crate::state::{Op, RunState};

/// The record format's version.
const RECORD_VERSION: u32 = 0;

/// How many event lines a run's log keeps before an operation trims it to half.
const EVENTS_KEPT: usize = 4000;

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

/// What the runner keeps about a run (never a secret's value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RunRecord {
    /// The record format's version.
    pub version: u32,
    /// The run's name.
    pub name: RunName,
    /// The manifest the run was started from, absolute.
    pub manifest: PathBuf,
    /// Its SHA-256.
    pub manifest_sha256: String,
    /// Its target.
    pub target: Target,
    /// Its driver.
    pub driver: Driver,
    /// Its cloud, for a cloud target's run: what the reaper picks runs by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<Cloud>,
    /// The Vagrant provider the run uses, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
    /// A record of a run never started yet.
    pub(crate) fn fresh(name: RunName, manifest: PathBuf, sha256: String, target: Target, driver: Driver) -> RunRecord {
        RunRecord {
            version: RECORD_VERSION,
            name,
            manifest,
            manifest_sha256: sha256,
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

/// The runner's state folder.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    runs: PathBuf,
}

impl Store {
    /// The store under `home`, its `runs/` folder created owner-only.
    pub(crate) fn open(home: &Path) -> Result<Store> {
        let runs = home.join("runs");
        private_dir(&runs).map_err(Error::io(&runs, "created"))?;
        Ok(Store { runs })
    }

    /// Run `name`'s folder, created owner-only.
    pub(crate) fn run_dir(&self, name: &RunName) -> Result<PathBuf> {
        let dir = self.runs.join(name.as_str());
        private_dir(&dir).map_err(Error::io(&dir, "created"))?;
        Ok(dir)
    }

    /// Run `name`'s lock file.
    pub(crate) fn lock_path(&self, name: &RunName) -> PathBuf {
        self.runs.join(name.as_str()).join("lock")
    }

    /// Run `name`'s record, when it has one.
    pub(crate) fn record(&self, name: &RunName) -> Result<Option<RunRecord>> {
        let path = self.runs.join(name.as_str()).join("record.json");
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| Error::io(&path, "read")(e.into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::io(&path, "read")(e)),
        }
    }

    /// Writes `record` whole.
    pub(crate) fn save(&self, record: &RunRecord) -> Result<()> {
        let path = self.run_dir(&record.name)?.join("record.json");
        let text = serde_json::to_vec_pretty(record).map_err(|e| Error::io(&path, "written")(e.into()))?;
        write_private(&path, &text).map_err(Error::io(&path, "written"))
    }

    /// Every run's record, by name.
    pub(crate) fn records(&self) -> Result<Vec<RunRecord>> {
        let entries = std::fs::read_dir(&self.runs).map_err(Error::io(&self.runs, "read"))?;
        let mut names: Vec<RunName> = entries.flatten().filter_map(|e| RunName::new(&e.file_name().to_string_lossy()).ok()).collect();
        names.sort();
        let mut records = Vec::new();
        for name in names {
            records.extend(self.record(&name)?);
        }
        Ok(records)
    }

    /// Run `name`'s event log, opened to append, trimmed to its newer half once it is long.
    pub(crate) fn event_log(&self, name: &RunName) -> Result<std::fs::File> {
        let path = self.run_dir(name)?.join("events.jsonl");
        let lines = read_lines(&path)?;
        if lines.len() > EVENTS_KEPT {
            let newer: Vec<&str> = lines.iter().skip(lines.len().saturating_sub(EVENTS_KEPT / 2)).map(String::as_str).collect();
            let kept = newer.join("\n") + "\n";
            write_private(&path, kept.as_bytes()).map_err(Error::io(&path, "written"))?;
        }
        fs::append_private(&path).map_err(Error::io(&path, "written"))
    }

    /// Run `name`'s logged events, oldest first (a line that doesn't parse is skipped).
    pub(crate) fn events(&self, name: &RunName) -> Result<Vec<Event>> {
        let path = self.runs.join(name.as_str()).join("events.jsonl");
        Ok(read_lines(&path)?.iter().filter_map(|l| serde_json::from_str(l).ok()).collect())
    }
}

/// `path`'s lines; none when it doesn't exist.
fn read_lines(path: &Path) -> Result<Vec<String>> {
    match std::fs::File::open(path) {
        Ok(f) => BufReader::new(f).lines().collect::<Result<_, _>>().map_err(Error::io(path, "read")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(Error::io(path, "read")(e)),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn record(name: &str) -> RunRecord {
        RunRecord::fresh(
            RunName::new(name).unwrap(),
            "/labs/x/.isoloom/docker/run.json".into(),
            "ab".repeat(32),
            Target::Docker,
            Driver::Compose,
        )
    }

    #[test]
    fn records_are_kept_per_run_and_listed_by_name() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        assert_eq!(store.record(&RunName::new("b").unwrap()).unwrap(), None);
        let mut b = record("b");
        b.params.insert("ISOLOOM_PUBLISH_FIXED".into(), "1".into());
        store.save(&b).unwrap();
        store.save(&record("a")).unwrap();
        std::fs::create_dir_all(home.path().join("runs").join("Not A Run")).unwrap();
        let names: Vec<String> = store.records().unwrap().iter().map(|r| r.name.to_string()).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(store.record(&b.name).unwrap(), Some(b));
    }

    #[test]
    fn the_event_log_is_trimmed_to_its_newer_half() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path()).unwrap();
        let name = RunName::new("r").unwrap();
        let mut log = store.event_log(&name).unwrap();
        for seq in 0..=EVENTS_KEPT {
            writeln!(log, "{{\"seq\":{seq}}}").unwrap();
        }
        drop(log);
        drop(store.event_log(&name).unwrap());
        let lines = read_lines(&home.path().join("runs/r/events.jsonl")).unwrap();
        assert_eq!(lines.len(), EVENTS_KEPT / 2);
        assert_eq!(lines.last().unwrap(), &format!("{{\"seq\":{EVENTS_KEPT}}}"));
    }
}
