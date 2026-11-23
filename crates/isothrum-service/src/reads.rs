//! A run's status reads, one at a time. Clients ask the status of one run at once (a page's
//! poll, another view of the same run, a read right after an operation): each asked the tools on
//! its own, so two `vagrant status` of the same machines could run together, and Vagrant refuses
//! a read of a machine another read holds. The reads of one run take turns, and a read that
//! waited for one that began after it was asked, with the same target and options, takes that
//! one's answer: as fresh as its own would have been, without asking the tools again. A failed
//! read stands for no other.
//!
//! The runner asks no tool while an operation holds the run ([`isothrum_runner::Status::live`]):
//! this is about the reads between operations.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use isothrum_client::proto::{OpOptions, Target};
use isothrum_runner::{RunName, Status};

use crate::engine::Refusal;

/// What a read was asked with: a read stands only for another asked the same.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Asked {
    /// Where the run is opened from.
    pub(crate) target: Option<Target>,
    /// The values and secrets its tools are given.
    pub(crate) options: OpOptions,
}

/// A run's last read: the tick it began at, what it was asked with, and its answer.
struct Last {
    began: u64,
    asked: Asked,
    status: Status,
}

/// One run's turn: its last read, held while a read runs.
type Turn = Arc<tokio::sync::Mutex<Option<Last>>>;

/// The status reads of the home's runs.
#[derive(Default)]
pub(crate) struct Reads {
    turns: Mutex<HashMap<RunName, Turn>>,
    ticks: AtomicU64,
}

impl Reads {
    /// The next tick: the order reads are asked for and begin in.
    fn tick(&self) -> u64 {
        self.ticks.fetch_add(1, Ordering::Relaxed)
    }

    /// Run `run`'s turn.
    fn turn(&self, run: &RunName) -> Turn {
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        Arc::clone(turns.entry(run.clone()).or_default())
    }

    /// Run `run`'s status, asked with `asked`: from `read` when its turn comes, or from the read
    /// that ended meanwhile when that one began after this was asked, with the same.
    ///
    /// # Errors
    ///
    /// `read`'s refusal.
    pub(crate) async fn status<F>(&self, run: &RunName, asked: Asked, read: impl FnOnce() -> F) -> Result<Status, Refusal>
    where
        F: Future<Output = Result<Status, Refusal>>,
    {
        let at = self.tick();
        let turn = self.turn(run);
        let mut last = turn.lock().await;
        if let Some(fresh) = last.as_ref().filter(|l| l.began > at && l.asked == asked) {
            tracing::debug!(%run, began = fresh.began, asked = at, "a status read stands for another");
            return Ok(fresh.status.clone());
        }
        let began = self.tick();
        let status = read().await?;
        *last = Some(Last {
            began,
            asked,
            status: status.clone(),
        });
        Ok(status)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    use isothrum_runner::ErrorCode;

    fn ready() -> Result<Status, Refusal> {
        let ready = serde_json::json!({ "state": "ready", "provider": null, "machines": [], "outputs": [] });
        serde_json::from_value(ready).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
    }

    fn asked(reveal: bool) -> Asked {
        Asked {
            target: None,
            options: OpOptions {
                reveal,
                ..OpOptions::default()
            },
        }
    }

    /// Reads that each take three seconds of Tokio's paused clock, counted: how many, and the
    /// most at once.
    #[derive(Default)]
    struct Counted {
        reads: AtomicUsize,
        running: AtomicUsize,
        most: AtomicUsize,
    }

    impl Counted {
        async fn read(&self) -> Result<Status, Refusal> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(3)).await;
            self.running.fetch_sub(1, Ordering::SeqCst);
            ready()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn reads_of_a_run_take_turns_and_one_asked_meanwhile_takes_the_next_ones_answer() {
        let (reads, counted) = (Reads::default(), Counted::default());
        let run = RunName::new("stack").expect("a name");
        let one = || reads.status(&run, asked(false), || counted.read());
        // A read in flight, then two more asked for while it runs.
        let (a, b, c) = tokio::join!(one(), one(), one());
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(counted.most.load(Ordering::SeqCst), 1, "never two at once");
        // The first began before the others were asked: the second reads again, and the third,
        // asked before that one began, takes its answer.
        assert_eq!(counted.reads.load(Ordering::SeqCst), 2);
        // Asked once every read ended: a read of its own.
        one().await.expect("a status");
        assert_eq!(counted.reads.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_read_stands_only_for_one_asked_the_same_and_a_failed_one_for_none() {
        let (reads, counted) = (Reads::default(), Counted::default());
        let run = RunName::new("stack").expect("a name");
        let (a, b, c) = tokio::join!(
            reads.status(&run, asked(false), || counted.read()),
            reads.status(&run, asked(false), || counted.read()),
            reads.status(&run, asked(true), || counted.read()),
        );
        assert!(a.is_ok() && b.is_ok() && c.is_ok());
        assert_eq!(counted.reads.load(Ordering::SeqCst), 3, "the revealing read asked for itself");
        let failed = reads.status(&run, asked(false), || async { Err(Refusal::new(ErrorCode::Io, "refused")) }).await;
        assert!(failed.is_err());
        reads.status(&run, asked(false), || counted.read()).await.expect("a read of its own");
        assert_eq!(counted.reads.load(Ordering::SeqCst), 4);
    }
}
