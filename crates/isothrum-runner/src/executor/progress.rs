//! A step's time limit, counted from its last progress: a slow download still moving runs on,
//! a stuck one repeating its line is stopped.
//!
//! Progress is an output line, on either stream, that isn't blank and isn't one of the last
//! [`RECENT`] distinct lines the step printed (compared whole, its surrounding spaces trimmed).
//! Lines are not normalized further: `Downloading 512MB/1.1GB` then `Downloading 513MB/1.1GB` is
//! progress, `36b0bafd7f97 Downloading 1.121GB` printed again is not, and neither is a stuck
//! multi-layer pull cycling through the same few lines. The step's limit is the time it may go
//! without progress, from its start; a step that prints nothing has its limit from its start,
//! as before. A step that keeps making progress (a script looping on changing lines) still stops
//! at [`CEILING`] from its start, or at its limit when that is longer.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use super::step::{cut, process_error};
use crate::Result;
use crate::error::{Error, TimedOut};
use crate::process::{Captured, ProcessError, ProcessRunner, ProcessSpec, capture_echoing};

/// The most any step runs, from its start, however much progress it makes, unless its own limit
/// is longer: six hours, the longest limit Isoloom gives a step today (a Windows run's `vagrant
/// up`), so no step stops sooner than it did when its limit counted from its start.
pub(crate) const CEILING: Duration = Duration::from_secs(6 * 3600);

/// How many of the last distinct lines a step printed a new line is compared with: enough for a
/// pull of many layers taking turns, few enough to keep nothing large.
const RECENT: usize = 64;

/// How long the last line an error quotes may be.
const LAST_LINE_MAX: usize = 300;

/// What a running step printed, as far as its time limit goes: when it last made progress, and
/// its last line. Shared by the step's line handler and its timer.
#[derive(Debug)]
pub(crate) struct Progress(Mutex<Seen>);

#[derive(Debug)]
struct Seen {
    /// When it last made progress (its start, until it does).
    at: Instant,
    /// Hashes of its last distinct lines, oldest first.
    recent: VecDeque<u64>,
    /// Its last non-blank line.
    last: Option<String>,
}

impl Progress {
    /// A step starting now.
    pub(crate) fn new() -> Progress {
        Progress(Mutex::new(Seen {
            at: Instant::now(),
            recent: VecDeque::with_capacity(RECENT),
            last: None,
        }))
    }

    /// The step printed `line`: progress when it is new.
    pub(crate) fn saw(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut hasher = DefaultHasher::new();
        line.hash(&mut hasher);
        let hash = hasher.finish();
        let mut seen = self.seen();
        seen.last = Some(line.to_owned());
        if seen.recent.contains(&hash) {
            return;
        }
        if seen.recent.len() == RECENT {
            seen.recent.pop_front();
        }
        seen.recent.push_back(hash);
        seen.at = Instant::now();
    }

    /// Runs `spec` with `runner`, keeping what it prints ([`capture_echoing`]), each line seen.
    pub(crate) async fn capture(&self, runner: &dyn ProcessRunner, spec: &ProcessSpec) -> Result<Captured, ProcessError> {
        capture_echoing(runner, spec, &mut |_, line| self.saw(line)).await
    }

    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `run`, for step `id`, given up (dropping it ends the process tree) once `progress` shows none
/// for `limit`, or at the ceiling: a timeout and a tool that couldn't run to its end are the
/// step's errors. `progress` is fed by `run`'s line handler.
pub(crate) async fn within<T>(id: &str, limit: Duration, progress: &Progress, run: impl Future<Output = Result<T, ProcessError>>) -> Result<T> {
    let ceiling = Instant::now() + limit.max(CEILING);
    tokio::pin!(run);
    loop {
        let stalled = progress.seen().at + limit;
        tokio::select! {
            biased;
            done = &mut run => return done.map_err(|e| process_error(id, e)),
            () = tokio::time::sleep_until(stalled.min(ceiling)) => {}
        }
        let now = Instant::now();
        let seen = progress.seen();
        let timed_out = if seen.at + limit <= now {
            TimedOut::NoProgress {
                after_s: limit.as_secs(),
                last_line: seen.last.as_deref().map(|l| cut(l, LAST_LINE_MAX)),
            }
        } else if now >= ceiling {
            TimedOut::Ceiling {
                after_s: limit.max(CEILING).as_secs(),
            }
        } else {
            continue;
        };
        return Err(Error::Timeout {
            step: id.to_owned(),
            timed_out,
        });
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
