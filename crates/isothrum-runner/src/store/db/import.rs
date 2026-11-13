//! The one-time import of a home a 2.x runner wrote: its files become rows, and are left
//! read-only beside the store, which the runner reads from then on.
//!
//! Per run folder (`runs/<name>/`): `record.json` becomes the run's row (its state as it was,
//! with no move recorded; what the user wants read from that state), `seen.json` its observation,
//! `events.jsonl` its events (as written, `v: 0`), and `anonymous-volumes` (the Compose volumes a
//! cut-short `down` or `update` recorded) live `volume` resources of an operation recorded as
//! abandoned, which the run's next operation finds. A record that can't be read is left where
//! it is, writable, and said: the import doesn't stop the home over one run.

use std::path::Path;

use super::{NewResource, OperationId, Outcome, Owner, ResourceKind, Tx};
use crate::Result;
use crate::error::ErrorCode;
use crate::name::RunName;
use crate::state::{Desired, Op, RunState};
use crate::store::{LastObserved, RunRecord};

/// The `store_meta` key set once the home's files were imported (or found absent).
pub(super) const IMPORTED: &str = "imported_v2";

/// The files of a 2.x run folder the import reads.
const RECORD: &str = "record.json";
const SEEN: &str = "seen.json";
const EVENTS: &str = "events.jsonl";
const VOLUMES: &str = "anonymous-volumes";

/// Imports `home`'s 2.x files, once: a home whose store already says so is left alone.
pub(super) fn from_files(tx: &mut Tx<'_>, home: &Path) -> Result<()> {
    if tx.meta(IMPORTED)?.is_some() {
        return Ok(());
    }
    let runs = home.join("runs");
    let mut imported = Vec::new();
    if let Some(entries) = crate::store::if_there(std::fs::read_dir(&runs)).map_err(crate::error::Error::io(&runs, "read"))? {
        let mut dirs: Vec<_> = entries.filter_map(std::result::Result::ok).map(|e| e.path()).collect();
        dirs.sort();
        for dir in dirs {
            if import_run(tx, &dir)? {
                imported.push(dir);
            }
        }
    }
    tx.set_meta(IMPORTED, &super::now())?;
    // Read-only once the rows are in: the transaction commits after this, and a failure to
    // commit leaves the files read-only and the store without them, which the next open
    // imports again (`imported_v2` isn't set): read-only files are still read.
    for dir in &imported {
        for file in [RECORD, SEEN, EVENTS, VOLUMES] {
            read_only(&dir.join(file));
        }
    }
    Ok(())
}

/// Imports the run folder `dir`: whether it held a record that was imported.
fn import_run(tx: &mut Tx<'_>, dir: &Path) -> Result<bool> {
    let Some(name) = dir.file_name().and_then(|n| n.to_str()).and_then(|n| RunName::new(n).ok()) else {
        return Ok(false);
    };
    let record: RunRecord = match crate::store::read_json(&dir.join(RECORD)) {
        Ok(Some(record)) => record,
        Ok(None) => return Ok(false),
        Err(e) => {
            tracing::warn!(run = %name, error = %e, "a 2.x run's record can't be read: the run isn't imported");
            return Ok(false);
        }
    };
    tx.save_record(&record, desired_of(record.state))?;
    if record.state == RunState::Failed {
        let cause = record.last.as_ref().and_then(|l| l.code);
        tx.import_state(&name, RunState::Failed, cause)?;
    }
    match crate::store::read_json::<LastObserved>(&dir.join(SEEN)) {
        Ok(Some(seen)) => drop(tx.observe(&name, &seen, record.state)?),
        Ok(None) => {}
        Err(e) => tracing::warn!(run = %name, error = %e, "a 2.x run's last observation can't be read: not imported"),
    }
    import_events(tx, &name, &dir.join(EVENTS))?;
    import_volumes(tx, &name, &dir.join(VOLUMES))?;
    Ok(true)
}

/// What the user wanted of a run a 2.x record left in `state`.
fn desired_of(state: RunState) -> Desired {
    match state {
        RunState::Parked | RunState::Parking => Desired::Parked,
        RunState::Absent | RunState::Destroying => Desired::Gone,
        _ => Desired::Running,
    }
}

/// The 2.x event log's lines, as written.
fn import_events(tx: &mut Tx<'_>, name: &RunName, path: &Path) -> Result<()> {
    let Some(text) = crate::store::if_there(std::fs::read(path)).map_err(crate::error::Error::io(path, "read"))? else {
        return Ok(());
    };
    for line in text.split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(line);
        let line = line.trim();
        if !line.is_empty() {
            tx.append_raw_event(Some(name.as_str()), None, line)?;
        }
    }
    Ok(())
}

/// The anonymous volumes a cut-short 2.x operation recorded: live rows of an abandoned
/// operation, so the run's next `down` or `update` removes them.
fn import_volumes(tx: &mut Tx<'_>, name: &RunName, path: &Path) -> Result<()> {
    let Some(text) = crate::store::if_there(std::fs::read_to_string(path)).map_err(crate::error::Error::io(path, "read"))? else {
        return Ok(());
    };
    let volumes: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    if volumes.is_empty() {
        return Ok(());
    }
    let by = abandoned(tx, name, Op::Down)?;
    for volume in volumes {
        let id = tx.creating(&NewResource::new(name, volume, ResourceKind::Volume, by).living(super::Lifetime::Run))?;
        tx.created(id, volume)?;
    }
    Ok(())
}

/// An operation journaled as begun and abandoned, for what a 2.x operation left.
fn abandoned(tx: &mut Tx<'_>, name: &RunName, op: Op) -> Result<OperationId> {
    let row = tx.begin_operation(name, op, &Owner::this_process(), None)?;
    tx.end_operation(row.id, Outcome::Abandoned, Some(ErrorCode::Cancelled))?;
    Ok(row.id)
}

/// Makes `path` read-only, when it is there; a failure is said, not fatal.
fn read_only(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    let mut permissions = meta.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(0o400);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(true);
    if let Err(e) = std::fs::set_permissions(path, permissions) {
        tracing::warn!(path = %path.display(), error = %e, "an imported 2.x file couldn't be made read-only");
    }
}
