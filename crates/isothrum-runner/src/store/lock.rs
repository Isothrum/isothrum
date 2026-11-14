//! One operation per run: this runner's own lock on it (an async mutex per run, held by the
//! [`crate::Runner`], never a process-wide static), then an OS lock on the run's `lock` file across
//! processes, which the OS releases when its holder dies. And one maker of the home's SSH key at a
//! time ([`hold`] on its lock file, `crate::ssh`).
//!
//! The order, everywhere: the run's lock first, and the `std` mutexes (the runner's flight and
//! reservations, an emitter's log) last and never across an `.await`. The SSH key's lock is taken
//! under the run's alone (or none). No path takes them in another order, so no circular wait.
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
const LOCK_WAIT: Duration = Duration::from_secs(1);

/// How often a held lock is tried again within [`LOCK_WAIT`].
const RETRY: Duration = Duration::from_millis(50);

/// The per-run locks of one runner. An entry lives while an operation holds or waits for its run:
/// the next acquire drops the ones nobody holds, so a long-lived runner touching many runs keeps
/// only those in use.
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
        let mutex = {
            let mut runs = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            // Only the map holds an entry nobody holds or waits for: dropped, it is made afresh
            // when its run is next acquired. Counted under the map's lock, which every clone is
            // taken under, so no acquire can be cloning it meanwhile.
            runs.retain(|_, held| Arc::strong_count(held) > 1);
            Arc::clone(runs.entry(run.clone()).or_default())
        };
        let here = mutex.lock_owned().await;
        let file = fs::lock_file(path).map_err(Error::io(path, "locked"))?;
        match locked_within(&file, LOCK_WAIT).await {
            Ok(true) => Ok(RunLock {
                _file: Held(file),
                _here: here,
            }),
            Ok(false) => Err(Error::Locked { run: run.to_string() }),
            Err(e) => Err(Error::io(path, "locked")(e)),
        }
    }
}

/// A lock file held for one short job on the home that two operations must not do at once (its
/// SSH key made); released when dropped.
#[derive(Debug)]
pub(crate) struct HomeLock {
    _file: Held,
}

/// Holds the OS lock on `path`, waiting up to `wait` while another handle holds it (another
/// process, another runner, another operation of this one); `None` when it was held all along.
pub(crate) async fn hold(path: &Path, wait: Duration) -> std::io::Result<Option<HomeLock>> {
    let file = fs::lock_file(path)?;
    Ok(locked_within(&file, wait).await?.then(|| HomeLock { _file: Held(file) }))
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

    #[tokio::test(start_paused = true)]
    async fn the_runs_no_operation_holds_are_let_go() {
        // Regression: the map kept an entry per run ever acquired, for the runner's lifetime.
        let dir = tempfile::tempdir().unwrap();
        let locks = RunLocks::default();
        for name in ["one", "two", "three"] {
            let name = RunName::new(name).unwrap();
            drop(locks.acquire(&name, &dir.path().join(format!("{name}.lock"))).await.unwrap());
        }
        let held = locks.acquire(&run(), &dir.path().join("lock")).await.unwrap();
        let kept: Vec<String> = locks.0.lock().unwrap().keys().map(ToString::to_string).collect();
        assert_eq!(kept, ["demo"], "only the run held now");
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn many_operations_on_a_few_runs_from_several_threads_hold_each_run_one_at_a_time() {
        // More tasks than threads, on runs whose entries come and go: each run is held by one
        // operation at a time, and every operation gets its turn.
        let dir = tempfile::tempdir().unwrap();
        let locks = Arc::new(RunLocks::default());
        let inside: Arc<[std::sync::atomic::AtomicUsize; 3]> = Arc::default();
        let mut ops = tokio::task::JoinSet::new();
        for i in 0..64 {
            let (locks, inside, dir) = (Arc::clone(&locks), Arc::clone(&inside), dir.path().to_path_buf());
            ops.spawn(async move {
                let which = i % 3;
                let name = RunName::new(&format!("run-{which}")).unwrap();
                let path = dir.join(format!("{name}.lock"));
                // Another operation of this runner may hold the run past the OS lock's wait:
                // waiting for this runner's own is unbounded, so it is never `locked`.
                let held = locks.acquire(&name, &path).await.unwrap();
                let now = inside[which].fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(now, 0, "two operations held {name} at once");
                tokio::time::sleep(Duration::from_millis(2)).await;
                inside[which].fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                drop(held);
            });
        }
        while let Some(done) = ops.join_next().await {
            done.unwrap();
        }
        let left = locks.0.lock().unwrap().values().filter(|m| Arc::strong_count(m) > 1).count();
        assert_eq!(left, 0, "nothing holds a run once every operation ended");
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
}
