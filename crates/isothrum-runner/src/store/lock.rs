//! One operation per run: this runner's own lock on it (an async mutex per run, held by the
//! [`crate::Runner`], never a process-wide static), then an OS lock on the run's `lock` file across
//! processes, which the OS releases when its holder dies.
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

/// The per-run locks of one runner.
#[derive(Debug, Default)]
pub(crate) struct RunLocks(Mutex<BTreeMap<RunName, Arc<tokio::sync::Mutex<()>>>>);

/// A run held for one operation; released when dropped.
#[derive(Debug)]
pub(crate) struct RunLock {
    _file: File,
    _here: OwnedMutexGuard<()>,
}

impl RunLocks {
    /// Holds `run`: waits for this runner's other operation on it, then takes the OS lock at
    /// `path`, failing with [`Error::Locked`] while another process holds it.
    pub(crate) async fn acquire(&self, run: &RunName, path: &Path) -> Result<RunLock> {
        let mutex = Arc::clone(self.0.lock().unwrap_or_else(PoisonError::into_inner).entry(run.clone()).or_default());
        let here = mutex.lock_owned().await;
        let file = fs::lock_file(path).map_err(Error::io(path, "locked"))?;
        let deadline = tokio::time::Instant::now() + GRACE;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    return Ok(RunLock { _file: file, _here: here });
                }
                Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(RETRY).await;
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(Error::Locked { run: run.to_string() });
                }
                Err(TryLockError::Error(source)) => {
                    return Err(Error::Io {
                        path: path.to_path_buf(),
                        action: "locked",
                        source,
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> RunName {
        RunName::new("lab").unwrap()
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
        // Regression (R2): a run lock that let the operation run unlocked when its file couldn't
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
}
