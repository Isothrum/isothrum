//! Tools cut short (their operation's future dropped: a cancel, Ctrl-C, a step's timeout) are
//! asked to stop before they are ended. On Unix the tool's process group gets SIGINT, which
//! Terraform, Vagrant and Docker Compose take as a request to stop cleanly (Terraform finishes
//! what it has in flight and saves its state), and SIGKILL once the tool has exited or after
//! [`GRACE`], whichever comes first, so nothing it started outlives it. Each one is waited for on
//! a thread of its own, since a drop can't wait: [`Stopping::stopped`] waits for them (a process
//! about to exit, such as the command line, calls it so none is left running), and
//! [`Stopping::end_now`] ends them at once (a second Ctrl-C).
//!
//! On Windows a tool runs headless (no console), so there is no console to send it Ctrl-Break,
//! and the call that sends one is raw FFI, which this crate forbids: its job object is
//! terminated at once.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use process_wrap::tokio::ChildWrapper;

/// How long a tool asked to stop has before it is ended.
pub(super) const GRACE: Duration = Duration::from_secs(30);
/// How often a stopping tool is looked at.
const POLL: Duration = Duration::from_millis(50);

/// The tools cut short that are still stopping.
#[derive(Debug)]
pub(super) struct Stopping {
    /// How long each has.
    grace: Duration,
    /// How many are still stopping.
    count: AtomicUsize,
    /// Raised by [`Stopping::end_now`]: a tool asked to stop before it is ended at once.
    epoch: AtomicU64,
    /// Woken when a stopping tool ended (the count went down).
    done: tokio::sync::Notify,
}

/// A spawned tool, handed to the thread that stops it.
type Slot = Arc<Mutex<Option<Box<dyn ChildWrapper>>>>;

impl Stopping {
    /// Tools given `grace` to stop.
    pub(super) fn new(grace: Duration) -> Stopping {
        Stopping {
            grace,
            count: AtomicUsize::new(0),
            epoch: AtomicU64::new(0),
            done: tokio::sync::Notify::new(),
        }
    }

    /// Asks `child`'s tree to stop, and ends it once it has, or after the grace. Where it can't
    /// be asked (Windows, a tree already gone), it is ended at once.
    pub(super) fn stop(self: &Arc<Self>, mut child: Box<dyn ChildWrapper>, recorded: Option<super::ledger::Recorded>) {
        if !interrupt(child.as_ref()) {
            end(child.as_mut());
            return;
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        let since = self.epoch.load(Ordering::SeqCst);
        let slot: Slot = Arc::new(Mutex::new(Some(child)));
        let (stopping, theirs) = (Arc::clone(self), Arc::clone(&slot));
        // Its entry in the run's tools goes once it has ended, not before: this process killed
        // meanwhile, the next operation ends it.
        let spawned = std::thread::Builder::new().name("isothrum-stop".into()).spawn(move || {
            if let Some(mut tool) = take(&theirs) {
                stopping.end_when_stopped(tool.as_mut(), since);
            }
            drop(recorded);
            stopping.count.fetch_sub(1, Ordering::SeqCst);
            stopping.done.notify_waiters();
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no thread to wait for a tool to stop: it is ended at once");
            if let Some(mut tool) = take(&slot) {
                end(tool.as_mut());
            }
            self.count.fetch_sub(1, Ordering::SeqCst);
            self.done.notify_waiters();
        }
    }

    /// Whether a tool is still stopping.
    pub(super) fn any(&self) -> bool {
        self.count.load(Ordering::SeqCst) > 0
    }

    /// Once no tool is stopping any more: woken by the last tool's end, not by polling (Clean
    /// Code ch.13, 2026-11-13). The wait is armed before the count is read, so an end between the
    /// two isn't missed.
    pub(super) async fn stopped(&self) {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.any() {
                return;
            }
            notified.await;
        }
    }

    /// Ends every tool still stopping, at once.
    pub(super) fn end_now(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Waits for `child` to exit, for the grace at most and until [`Stopping::end_now`] (from
    /// epoch `since` on); then ends what is left of its tree.
    fn end_when_stopped(&self, child: &mut dyn ChildWrapper, since: u64) {
        let deadline = Instant::now() + self.grace;
        while Instant::now() < deadline && self.epoch.load(Ordering::SeqCst) == since && !exited(child) {
            std::thread::sleep(POLL);
        }
        end(child);
    }
}

/// The child in `slot`, taken out.
fn take(slot: &Slot) -> Option<Box<dyn ChildWrapper>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Whether the tool has exited (or can't be waited for any more).
fn exited(child: &mut dyn ChildWrapper) -> bool {
    !matches!(child.try_wait(), Ok(None))
}

/// SIGINT to the tool's process group; whether it was sent.
#[cfg(unix)]
fn interrupt(child: &dyn ChildWrapper) -> bool {
    /// SIGINT's number, the same on every Unix (POSIX).
    const SIGINT: i32 = 2;
    tracing::debug!(pid = ?child.id(), "asking a process tree cut short to stop");
    match child.signal(SIGINT) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "the process tree had already ended");
            false
        }
    }
}

/// Nothing to send on Windows (see the module's documentation).
#[cfg(windows)]
fn interrupt(_child: &dyn ChildWrapper) -> bool {
    false
}

/// SIGKILL to the process group, or the job object terminated: nothing the tool started outlives
/// it. Already gone is fine.
fn end(child: &mut dyn ChildWrapper) {
    tracing::debug!(pid = ?child.id(), "ending a process tree cut short");
    if let Err(e) = child.start_kill() {
        tracing::debug!(error = %e, "the process tree had already ended");
    }
}
