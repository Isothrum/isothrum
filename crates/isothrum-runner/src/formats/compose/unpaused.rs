//! What a resume waits for once it unpaused services: their health checks run again.

use std::time::Duration;

use super::READ_LIMIT;
use super::failures::{checked, engine_error};
use crate::Result;
use crate::cli::compose::{self, Project};
use crate::formats::ToolCx;

/// How long a resume waits for the services it unpaused to read healthy again before its `up`.
const UNPAUSED_HEALTH: Duration = Duration::from_secs(60);

/// How often a resume reads the unpaused services' health while it waits.
const HEALTH_POLL: Duration = Duration::from_secs(1);

/// Waits until none of `services`, just unpaused, reads unhealthy, at most [`UNPAUSED_HEALTH`].
/// Docker reads a paused container unhealthy, and it stays so until its health check next runs
/// (an interval after the unpause): an `up --wait` then fails on a dependency that is fine
/// (`dependency failed to start: container … is unhealthy`). One unhealthy past the wait is left
/// for the `up` to judge.
pub(super) async fn health_back(cx: &ToolCx<'_>, project: &Project<'_>, services: &[String]) -> Result<()> {
    let deadline = tokio::time::Instant::now() + UNPAUSED_HEALTH;
    loop {
        let containers = checked(cx, compose::ps(&cx.read(READ_LIMIT), project).await, |e| engine_error("status", e)).await?;
        let waiting = containers
            .iter()
            .any(|c| !c.one_off() && c.is_unhealthy() && services.iter().any(|s| s == c.service()));
        if !waiting || tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
}
