//! An `ssh.run` step with a `wait_s`: its host may still be booting (a cloud VM right after the
//! apply that made it), so a run that ssh itself couldn't connect for (exit 255, nothing printed
//! by the command) is tried again every [`RETRY`] until `wait_s` is spent. Only the connection
//! is retried: a command that ran and failed fails the step at once. ssh's own complaints are
//! held back while it can't connect, so the step's log is its command's.

use std::time::Duration;

use isoloom_manifest::Step;

use super::Cx;
use super::step::{Hold, once};
use crate::Result;
use crate::error::Error;
use crate::events::WarningCode;
use crate::ssh::Wait;

/// How long between two tries.
const RETRY: Duration = Duration::from_secs(5);

/// ssh's own exit code when it couldn't connect (or the command exited 255).
const SSH_FAILED: i32 = 255;

/// Runs `step` until its host answers within `wait`, then as any step.
pub(crate) async fn run(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send), wait: &Wait) -> Result<()> {
    let deadline = tokio::time::Instant::now() + wait.limit;
    let mut said = false;
    loop {
        let mut ended = once(cx, list, step, observe, Hold::UntilOutput).await?;
        let unreachable = ended.last.exit.code == Some(SSH_FAILED) && !ended.printed;
        if !unreachable {
            ended.show_held(cx, step, observe);
            return ended.result(cx, step);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Unreachable {
                step: step.id.to_string(),
                login: wait.login.clone(),
                after_s: wait.limit.as_secs(),
                reason: cx.events.mask(ended.last.stderr.join(" ").trim()),
            });
        }
        if !said {
            let note = format!("{} doesn't answer over SSH yet; trying again for up to {}s", wait.login, wait.limit.as_secs());
            cx.events.warning(WarningCode::Retrying, &note);
            said = true;
        }
        tokio::time::sleep(RETRY).await;
    }
}
