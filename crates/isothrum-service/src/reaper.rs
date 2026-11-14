//! The service's reaper, in its own process: every so often, the recorded runs whose lifetime
//! passed ([`Engine::expired`]) get their `down` queued, as a client's would be, with the values
//! the run's record keeps (a secret is never kept: a run whose `down` needs one fails, and is
//! tried again later, or reaped by `isothrum reap` given it). A run with an operation queued or
//! running is left until it ends; one whose `down` was queued is not queued again before
//! [`RETRY`], so a `down` that keeps failing doesn't run back to back.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use isothrum_client::proto::{Command, OpOptions};
use isothrum_runner::RunName;
use tokio::time::Instant;

use crate::engine::Engine;
use crate::queue::Queue;

/// How often the reaper looks, by default.
pub(crate) const EVERY: Duration = Duration::from_secs(60);

/// How long before a run whose `down` the reaper queued is queued again.
pub(crate) const RETRY: Duration = Duration::from_secs(15 * 60);

/// Looks every `every`, until the task is aborted.
pub(crate) async fn reap(engine: Arc<dyn Engine>, queue: Arc<Queue>, every: Duration) {
    let mut tried = BTreeMap::new();
    let mut tick = tokio::time::interval(every);
    loop {
        tick.tick().await;
        sweep(engine.as_ref(), &queue, &mut tried, Instant::now());
    }
}

/// One look at `now`: each expired run's `down` queued, unless it has work or was tried within
/// [`RETRY`] (`tried`: when each run's was last queued). The runs queued.
pub(crate) fn sweep(engine: &dyn Engine, queue: &Arc<Queue>, tried: &mut BTreeMap<RunName, Instant>, now: Instant) -> Vec<RunName> {
    let expired = match engine.expired() {
        Ok(expired) => expired,
        Err(refusal) => {
            tracing::warn!(error = %refusal.message, "the reaper couldn't read the expired runs");
            return Vec::new();
        }
    };
    tried.retain(|_, at| now.duration_since(*at) < RETRY);
    let mut queued = Vec::new();
    for target in expired {
        let Some(name) = target.name.clone() else { continue };
        if tried.contains_key(&name) || queue.has_work(&name) {
            continue;
        }
        let down = Command::Down {
            target,
            options: OpOptions::default(),
        };
        match queue.submit(down) {
            Ok((operation, run, _)) => {
                tracing::info!(run = %run, %operation, "the reaper queued the down of a run past its lifetime");
                tried.insert(name, now);
                queued.push(run);
            }
            Err(refusal) => tracing::warn!(run = %name, error = %refusal.message, "the reaper couldn't queue a run's down"),
        }
    }
    queued
}
