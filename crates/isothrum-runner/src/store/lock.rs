//! One operation per run: this runner's own lock on it (an async mutex per run, held by the
//! [`crate::Runner`], never a process-wide static), then an OS lock on the run's `lock` file across
//! processes, which the OS releases when its holder dies. And one slot pick per home at a time:
//! an OS lock on the home's `slots.lock`, held while a start picks its slot and records it, so
//! two starts in one home (two processes, two runners) never take the same slot.
//!
//! The lock fails **closed**: a lock file that can't be opened or locked stops the operation. A lock
//! that carries on without it would allow what the lock is there to prevent (Terraform's own state lock is cleared on the assumption that this one is held).

use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::OwnedMutexGuard;

use super::fs;
use crate::Result;
use crate::error::Error;
use crate::name::RunName;

/// How long a lock another process holds is waited for before the run reads as busy: a holder
/// that just finished, or a process started meanwhile that briefly shares the lock's handle
/// between its fork and its exec, lets go within it.
const GRACE: Duration = Duration::from_secs(1);

/// How often a held lock is tried again within [`GRACE`].
const RETRY: Duration = Duration::from_millis(50);

/// How long a start waits for another one's slot pick: a pick takes a few file reads and one
/// write, so a lock held this long is held by a process that hangs.
const SLOTS_WAIT: Duration = Duration::from_secs(30);

/// The per-run locks of one runner.
#[derive(Debug, Default)]
pub(crate) struct RunLocks(Mutex<BTreeMap<RunName, Arc<tokio::sync::Mutex<()>>>>);

/// A run held for one operation; released when dropped, the OS lock before this runner's.
#[derive(Debug)]
pub(crate) struct RunLock {
    _file: Held,
    _here: OwnedMutexGuard<()>,
}

/// A lock file this process holds the OS lock on; unlocked when dropped, then closed.
///
/// Closing the file alone doesn't free it: on Unix, a process spawned while it is open (a tool
/// another operation of this runner starts, at any time) gets a copy of the handle, which holds the
/// lock until the process execs and drops it. Freed by the close only, the lock outlived the
/// operation that held it, and a caller trying again at once found the run `locked`. Unlocked, it
/// is free whoever still has a copy.
#[derive(Debug)]
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        if let Err(e) = self.0.unlock() {
            // Closed next: the lock is freed once the copies spawned meanwhile are gone.
            tracing::warn!(error = %e, "a lock file wasn't unlocked; it is freed when closed");
        }
    }
}

impl RunLocks {
    /// Holds `run`: waits for this runner's other operation on it, then takes the OS lock at
    /// `path`, failing with [`Error::Locked`] while another process holds it.
    pub(crate) async fn acquire(&self, run: &RunName, path: &Path) -> Result<RunLock> {
        let mutex = Arc::clone(self.0.lock().unwrap_or_else(PoisonError::into_inner).entry(run.clone()).or_default());
        let here = mutex.lock_owned().await;
        let file = fs::lock_file(path).map_err(Error::io(path, "locked"))?;
        match locked_within(&file, GRACE).await {
            Ok(true) => Ok(RunLock {
                _file: Held(file),
                _here: here,
            }),
            Ok(false) => Err(Error::Locked { run: run.to_string() }),
            Err(e) => Err(Error::io(path, "locked")(e)),
        }
    }
}

/// The home's slots, held for one start's pick; released when dropped.
#[derive(Debug)]
pub(crate) struct SlotsLock {
    _file: Held,
}

/// Holds the home's slots (the OS lock on `path`), waiting for another start's pick.
///
/// # Errors
///
/// `io`, when the lock file can't be opened or locked; `locked` ([`Error::SlotsLocked`]) when
/// another start held it for [`SLOTS_WAIT`].
pub(crate) async fn hold_slots(path: &Path) -> Result<SlotsLock> {
    let file = fs::lock_file(path).map_err(Error::io(path, "locked"))?;
    match locked_within(&file, SLOTS_WAIT).await {
        Ok(true) => Ok(SlotsLock { _file: Held(file) }),
        Ok(false) => Err(Error::SlotsLocked {
            waited_s: SLOTS_WAIT.as_secs(),
        }),
        Err(e) => Err(Error::io(path, "locked")(e)),
    }
}

/// Locks `file`, trying again until `wait` has passed while another handle holds it: whether it
/// was locked.
async fn locked_within(file: &File, wait: Duration) -> std::io::Result<bool> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => tokio::time::sleep(RETRY).await,
            Err(TryLockError::WouldBlock) => return Ok(false),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> RunName {
        RunName::new("demo").unwrap()
    }

    #[tokio::test]
    async fn another_process_holding_the_run_keeps_it_busy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        // A handle of its own stands in for the other process (an OS lock conflicts across handles
        // just as across processes).
        let other = File::create(&path).unwrap();
        other.lock().unwrap();
        let locks = RunLocks::default();
        assert!(matches!(locks.acquire(&run(), &path).await, Err(Error::Locked { .. })));
        drop(other);
        let held = locks.acquire(&run(), &path).await.unwrap();
        drop(held);
    }

    #[tokio::test]
    async fn a_lock_that_cant_be_taken_fails_closed() {
        // Regression: a run lock that let the operation run unlocked when its file couldn't
        // be opened. Here the lock's place is a folder: the operation must not start.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        std::fs::create_dir_all(&path).unwrap();
        let err = RunLocks::default().acquire(&run(), &path).await.unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::Io, "{err}");
        let missing = dir.path().join("gone").join("lock");
        assert!(RunLocks::default().acquire(&run(), &missing).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_operation_of_this_runner_waits_for_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let locks = Arc::new(RunLocks::default());
        let first = locks.acquire(&run(), &path).await.unwrap();
        let waiting = tokio::spawn({
            let (locks, path) = (Arc::clone(&locks), path.clone());
            async move { locks.acquire(&run(), &path).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiting.is_finished(), "waits while the first holds the run");
        drop(first);
        waiting.await.unwrap().unwrap();
    }

    /// A copy of `held`'s handle, as a process spawned while it was held has until it execs.
    fn spawned_meanwhile(Held(file): &Held) -> File {
        file.try_clone().unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn a_run_released_is_free_at_once_while_a_process_spawned_meanwhile_still_has_its_handle() {
        // Regression: a run's lock was freed only by closing its file, so it outlived the
        // operation while a tool spawned meanwhile, by another operation, still had the handle:
        // the next operation on the run, at once, was refused as `locked`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let locks = RunLocks::default();
        let held = locks.acquire(&run(), &path).await.unwrap();
        let RunLock { _file: file, .. } = &held;
        let spawned = spawned_meanwhile(file);
        drop(held);
        let again = locks.acquire(&run(), &path).await;
        assert!(again.is_ok(), "the run is free once released: {again:?}");
        drop(spawned);
    }

    #[tokio::test(start_paused = true)]
    async fn the_slots_released_are_free_at_once_while_a_process_spawned_meanwhile_still_has_their_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slots.lock");
        let held = hold_slots(&path).await.unwrap();
        let SlotsLock { _file: file } = &held;
        let spawned = spawned_meanwhile(file);
        drop(held);
        let again = hold_slots(&path).await;
        assert!(again.is_ok(), "the slots are free once released: {again:?}");
        drop(spawned);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slot_pick_waits_for_another_processes_then_takes_the_slots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slots.lock");
        let other = File::create(&path).unwrap();
        other.lock().unwrap();
        let waiting = tokio::spawn({
            let path = path.clone();
            async move { hold_slots(&path).await.map(drop) }
        });
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert!(!waiting.is_finished(), "waits while another process picks");
        drop(other);
        waiting.await.unwrap().unwrap();
        let held = hold_slots(&path).await.unwrap();
        let stuck = hold_slots(&path).await.unwrap_err();
        assert_eq!(stuck.code(), crate::ErrorCode::Locked, "busy, not an I/O failure: {stuck}");
        assert_eq!(stuck.to_string(), "another start has held this home's slot pick for 30 s; it may hang");
        drop(held);
    }
}
