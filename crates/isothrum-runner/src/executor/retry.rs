//! A download that failed for the network, tried again. A registry or a box catalog's connection
//! reset in the middle of a large download (`connection reset by peer`, an `i/o timeout`, a
//! gateway's 502) fails the download, and the start with it, though the next try would most likely
//! succeed: Docker keeps the layers it finished, so a new pull takes up where the last one ended.
//! Such a failure ([`Failure::Passing`], as `tool_output::download` reads it) runs again, [`BACKOFF`] apart, each new try said in a
//! `retrying` warning, as the runner's other retries are; any other failure (a registry refusing a sign-in, an image or a box it
//! doesn't have, its rate limit) is the download's at once, as is a download that stalled. The
//! wait is the operation's: an operation cut short stops waiting with it.

use std::time::Duration;

use super::Cx;
use crate::events::WarningCode;
use crate::tool_output::download::Failure;

/// How long before each new try of a download that failed for the network: as many tries again as
/// it holds, after the first.
pub(crate) const BACKOFF: [Duration; 3] = [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(45)];

/// The tries of one download.
#[derive(Debug, Default)]
pub(crate) struct Retry {
    /// How many times it was tried again so far.
    again: usize,
}

impl Retry {
    /// Whether download `id`, which failed with `reason` (masked), runs again: when `reason` shows
    /// a network error that passes ([`Failure::Passing`]) and [`BACKOFF`] holds another wait, says so in
    /// a `retrying` warning (the step, which try comes, out of how many, and the line that
    /// failed), then waits before it.
    pub(crate) async fn again(&mut self, cx: &Cx<'_>, id: &str, reason: &str) -> bool {
        let Failure::Passing(line) = Failure::of(reason) else { return false };
        let Some(wait) = BACKOFF.get(self.again).copied() else { return false };
        self.again += 1;
        let (next, of) = (self.again + 1, BACKOFF.len() + 1);
        tracing::debug!(step = id, %line, "a download failed for the network: tried again");
        cx.events.warning(
            WarningCode::Retrying,
            &format!(
                "step `{id}`: download failed for the network, retrying in {}s (attempt {next} of {of}): {line}",
                wait.as_secs()
            ),
        );
        tokio::time::sleep(wait).await;
        true
    }
}
