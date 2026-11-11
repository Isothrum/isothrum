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
//! at [`CEILING`] from its start, or at its limit when that is longer. A download a step makes in
//! its own phase before it runs (a Compose start's images, [`downloading`]) has no ceiling: it is
//! stopped only once it shows no progress for as long as a download may.

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

/// How long a download may go without progress before it counts as stuck: a Compose start's
/// images, a Vagrant run's boxes.
pub(crate) const DOWNLOAD_STALL: Duration = Duration::from_secs(15 * 60);

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
    match watched(limit, Some(limit.max(CEILING)), progress, run).await {
        Ok(done) => done.map_err(|e| process_error(id, e)),
        Err(timed_out) => Err(Error::Timeout {
            step: id.to_owned(),
            timed_out,
        }),
    }
}

/// `run`, a download for step `id`, given up once `progress` shows none for `stall`, with no
/// ceiling: a slow download still moving runs as long as it moves. A stall is the step's
/// `download_stalled`, a tool that couldn't run to its end its error as for [`within`].
pub(crate) async fn downloading<T>(id: &str, stall: Duration, progress: &Progress, run: impl Future<Output = Result<T, ProcessError>>) -> Result<T> {
    match watched(stall, None, progress, run).await {
        Ok(done) => done.map_err(|e| process_error(id, e)),
        Err(TimedOut::NoProgress { after_s, last_line }) => Err(Error::DownloadStalled {
            step: id.to_owned(),
            after_s,
            last_line,
        }),
        // No ceiling was set: only a stall ends it.
        Err(other) => Err(Error::Timeout {
            step: id.to_owned(),
            timed_out: other,
        }),
    }
}

/// `run`, until it ends, or until `progress` shows none for `limit`, or until `ceiling` from now
/// when one is set: which limit it ran into, then.
async fn watched<T>(limit: Duration, ceiling: Option<Duration>, progress: &Progress, run: impl Future<Output = T>) -> Result<T, TimedOut> {
    let deadline = ceiling.map(|c| (Instant::now() + c, c));
    tokio::pin!(run);
    loop {
        let stalled = progress.seen().at + limit;
        let wake = deadline.map_or(stalled, |(at, _)| stalled.min(at));
        tokio::select! {
            biased;
            done = &mut run => return Ok(done),
            () = tokio::time::sleep_until(wake) => {}
        }
        let now = Instant::now();
        let seen = progress.seen();
        if seen.at + limit <= now {
            return Err(TimedOut::NoProgress {
                after_s: limit.as_secs(),
                last_line: seen.last.as_deref().map(|l| cut(l, LAST_LINE_MAX)),
            });
        }
        if let Some((at, after)) = deadline
            && now >= at
        {
            return Err(TimedOut::Ceiling { after_s: after.as_secs() });
        }
    }
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
