//! The runner's state, one folder per run under the embedder's home:
//!
//! ```text
//! <home>/slots.lock    the OS lock a start holds while it picks its slot (`${run.slot}`)
//! <home>/runs/<run name>/
//!   record.json     the run's record (atomic, owner-only)
//!   lock            the OS lock: one operation per run across processes
//!   events.jsonl    the last operations' events, so a reattached UI can replay them
//!   …               what the manifest keeps in `${run.state_dir}` (Compose's pinned ports)
//! ```
//!
//! The run's folder holds only the run and its generated files, so replacing it never strands a
//! run. `Store` is a struct over the file system, not a trait: tests use a temporary home.

#[cfg(any(windows, test))]
mod acl;
mod fs;
mod lock;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use isoloom_manifest::{Cloud, Driver, Target};
use serde::{Deserialize, Serialize};

pub(crate) use fs::{create_private, private_dir, remove_path, write_private};
pub(crate) use lock::{RunLocks, SlotsLock};

use crate::Result;
use crate::error::{Error, ErrorCode};
use crate::events::EventLog;
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

/// The runner's state folder.
#[derive(Debug, Clone)]
pub(crate) struct Store {
    runs: PathBuf,
    /// The home's `slots.lock`: one slot pick at a time.
    slots_lock: PathBuf,
}

impl Store {
    /// The store under `home` (owner-only on Windows, see [`fs::private_home`]), its `runs/`
    /// folder created owner-only.
    pub(crate) fn open(home: &Path, profile: Option<&Path>) -> Result<Store> {
        fs::private_home(home, profile).map_err(Error::io(home, "made owner-only"))?;
        let runs = home.join("runs");
        private_dir(&runs).map_err(Error::io(&runs, "created"))?;
        Ok(Store {
            runs,
            slots_lock: home.join("slots.lock"),
        })
    }

    /// Holds the home's slots while a start picks its slot and records it.
    pub(crate) async fn hold_slots(&self) -> Result<SlotsLock> {
        lock::hold_slots(&self.slots_lock).await
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
        read_json(&self.runs.join(name.as_str()).join("record.json"))
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
            let mut kept = Vec::new();
            for line in lines.iter().skip(lines.len().saturating_sub(EVENTS_KEPT / 2)) {
                kept.extend_from_slice(line);
                kept.push(b'\n');
            }
            write_private(&path, &kept).map_err(Error::io(&path, "written"))?;
        }
        fs::append_private(&path).map_err(Error::io(&path, "written"))
    }

    /// Run `name`'s logged events, oldest first, with how many of its lines aren't an event
    /// this runner reads (a line cut short, mid-character included).
    pub(crate) fn events(&self, name: &RunName) -> Result<EventLog> {
        let path = self.runs.join(name.as_str()).join("events.jsonl");
        let mut log = EventLog::default();
        for line in read_lines(&path)?.iter().filter(|l| !l.trim_ascii().is_empty()) {
            match serde_json::from_slice(line) {
                Ok(event) => log.events.push(event),
                Err(_) => log.unreadable += 1,
            }
        }
        Ok(log)
    }
}

/// `path`'s lines, as bytes: a log cut short (a full disk, a killed process) can end mid-character,
/// and that line is one like any other. None when the file doesn't exist.
fn read_lines(path: &Path) -> Result<Vec<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(path, "read")(e)),
    };
    let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    Ok(body
        .split(|b| *b == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .collect())
}

/// The JSON file at `path`, read; `None` when there is none.
///
/// # Errors
///
/// It exists but can't be read, or doesn't hold a `T` (`InvalidData`).
pub(crate) fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| Error::io(path, "read")(e.into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
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
            "/work/x/.isoloom/docker/run.json".into(),
            Target::Docker,
            Driver::Compose,
        )
    }

    #[test]
    fn records_are_kept_per_run_and_listed_by_name() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path(), None).unwrap();
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

    /// A store in a home of its own, and run `r`.
    fn run_r() -> (tempfile::TempDir, Store, RunName) {
        let home = tempfile::tempdir().unwrap();
        let store = Store::open(home.path(), None).unwrap();
        (home, store, RunName::new("r").unwrap())
    }

    #[test]
    fn the_event_log_is_trimmed_to_its_newer_half() {
        let (home, store, name) = run_r();
        let mut log = store.event_log(&name).unwrap();
        for seq in 0..=EVENTS_KEPT {
            writeln!(log, "{{\"seq\":{seq}}}").unwrap();
        }
        drop(log);
        drop(store.event_log(&name).unwrap());
        let lines = read_lines(&home.path().join("runs/r/events.jsonl")).unwrap();
        assert_eq!(lines.len(), EVENTS_KEPT / 2);
        assert_eq!(lines.last().unwrap(), format!("{{\"seq\":{EVENTS_KEPT}}}").as_bytes());
    }

    #[test]
    fn an_event_log_line_this_runner_cant_read_is_counted_not_dropped_unsaid() {
        let (_home, store, name) = run_r();
        let mut log = store.event_log(&name).unwrap();
        let read = r#"{"v":0,"seq":1,"time":"2026-10-15T12:00:00Z","run":"r","op":"up","event":"result","ok":true,"data":null}"#;
        // A 1.x warning code this runner no longer has, and a line cut short.
        let older = r#"{"v":0,"seq":2,"time":"2026-10-15T12:00:01Z","run":"r","op":"up","event":"warning","code":"hook_failed","message":"x"}"#;
        writeln!(log, "{read}\n{older}\n{{\"v\":0,\"se\n").unwrap();
        drop(log);
        let events = store.events(&name).unwrap();
        assert_eq!((events.events.len(), events.unreadable), (1, 2));
    }

    /// A log cut mid-character (a full disk, a killed process) holds a line that isn't UTF-8: it
    /// is one unreadable line, and the log still opens, is read and is trimmed.
    #[test]
    fn a_line_cut_mid_character_is_unreadable_and_the_log_goes_on() {
        let (home, store, name) = run_r();
        let read = r#"{"v":0,"seq":1,"time":"2026-10-15T12:00:00Z","run":"r","op":"up","event":"result","ok":true,"data":null}"#;
        let mut log = store.event_log(&name).unwrap();
        log.write_all(format!("{read}\n").as_bytes()).unwrap();
        // `é` is 0xC3 0xA9: the line ends after its first byte.
        log.write_all(b"{\"v\":0,\"message\":\"d\xC3").unwrap();
        drop(log);
        let mut reopened = store.event_log(&name).expect("the log opens to append");
        reopened.write_all(format!("\n{read}\n").as_bytes()).unwrap();
        drop(reopened);
        let events = store.events(&name).unwrap();
        assert_eq!((events.events.len(), events.unreadable), (2, 1));

        let mut many = store.event_log(&name).unwrap();
        for seq in 0..=EVENTS_KEPT {
            writeln!(many, "{{\"seq\":{seq}}}").unwrap();
        }
        drop(many);
        drop(store.event_log(&name).expect("a long log with such a line is trimmed"));
        assert_eq!(read_lines(&home.path().join("runs/r/events.jsonl")).unwrap().len(), EVENTS_KEPT / 2);
    }

    #[test]
    fn a_files_lines_are_split_on_line_feeds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        assert_eq!(read_lines(&path).unwrap(), Vec::<Vec<u8>>::new(), "no file, no line");
        for (text, lines) in [
            (&b""[..], vec![]),
            (b"a\n", vec![b"a".to_vec()]),
            (b"a\r\nb", vec![b"a".to_vec(), b"b".to_vec()]),
            (b"a\n\nb\n", vec![b"a".to_vec(), b"".to_vec(), b"b".to_vec()]),
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(read_lines(&path).unwrap(), lines, "{text:?}");
        }
    }
}
