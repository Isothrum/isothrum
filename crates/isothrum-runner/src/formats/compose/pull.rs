//! A start's images, pulled before its `up` (`pull --ignore-buildable`), so a pull is watched on
//! its own: one that prints nothing for [`PULL_STALL`] is stopped as `download_stalled`, one the
//! network cut short is tried again ([`RETRY_AFTER`]), and one the registry refused (no such
//! image or tag, a sign-in it needs) is `download_failed` at once, never tried again.

use std::time::Duration;

use super::{Compose, ToolCx, engine_error};
use crate::Result;
use crate::cli::CliError;
use crate::cli::compose::{self, Failure};
use crate::error::{DownloadKind, Error};
use crate::events::WarningCode;

/// How long a pull may print nothing before it is stopped: a layer's progress is a line, so a
/// slow pull that moves runs on.
pub(super) const PULL_STALL: Duration = Duration::from_secs(600);

/// How long a pull the network cut short waits before each try again: two more tries, the last
/// one's failure the start's.
pub(super) const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(10), Duration::from_secs(30)];

/// How long a pull may take at most, moving or not.
const PULL_LIMIT: Duration = Duration::from_secs(3600);

/// The step a pull's failure names.
const STEP: &str = "pull-up";

impl Compose {
    /// Pulls the images the run's start runs, as the module says.
    ///
    /// # Errors
    ///
    /// `download_stalled` when a pull printed nothing for [`PULL_STALL`]; `download_failed` when
    /// the registry refused an image, or the network cut every try short; any other tool error.
    pub(super) async fn pull(&self, cx: &ToolCx<'_>) -> Result<()> {
        let call = cx.call(PULL_LIMIT);
        let project = Self::project(cx);
        let mut waits = RETRY_AFTER.iter();
        loop {
            match compose::pull(&call, &project, PULL_STALL).await {
                Ok(()) => return Ok(()),
                Err(CliError::Failed {
                    failure: Failure::PullTransient(reason),
                    ..
                }) => {
                    let Some(wait) = waits.next() else {
                        return Err(Error::DownloadFailed {
                            what: DownloadKind::Images,
                            step: STEP.to_owned(),
                            reason,
                        });
                    };
                    let said = format!("an image's pull was cut short ({reason}): trying again in {}s", wait.as_secs());
                    (cx.warn)(WarningCode::Retrying, &said);
                    tokio::time::sleep(*wait).await;
                }
                Err(CliError::Failed {
                    failure: Failure::PullRefused(reason),
                    ..
                }) => {
                    return Err(Error::DownloadFailed {
                        what: DownloadKind::Images,
                        step: STEP.to_owned(),
                        reason,
                    });
                }
                Err(e) => return Err(engine_error(STEP, e)),
            }
        }
    }
}
