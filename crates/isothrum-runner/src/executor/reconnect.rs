//! An `ssh.run` step with a `wait_s`: its host may still be booting (a cloud VM right after the
//! apply that made it), so a run that ssh itself couldn't connect for (exit 255, nothing printed
//! by the command: [`ssh::unreachable`]) is tried again every [`ssh::RETRY`] until `wait_s` is
//! spent ([`ssh::Retrying`]). Only the connection is retried: a command that ran and failed fails
//! the step at once. ssh's own complaints are held back while it can't connect, so the step's log
//! is its command's.

use isoloom_manifest::Step;

use super::Cx;
use super::step::{Ended, Hold, once};
use crate::Result;
use crate::error::Error;
use crate::events::WarningCode;
use crate::ssh::{self, Wait};

/// Runs `step` until its host answers within `wait`, then as any step.
pub(crate) async fn run(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send), wait: &Wait) -> Result<()> {
    let deadline = tokio::time::Instant::now() + wait.limit;
    let unreachable = |e: &Ended| ssh::unreachable(e.last.exit, e.printed);
    let note = format!("{} doesn't answer over SSH yet; trying again for up to {}s", wait.login, wait.limit.as_secs());
    let mut retrying = ssh::Retrying::until(deadline);
    let mut ended = loop {
        let ended = once(cx, list, step, observe, Hold::UntilOutput).await?;
        if !retrying.again(unreachable(&ended), || cx.events.warning(WarningCode::Retrying, &note)).await {
            break ended;
        }
    };
    if unreachable(&ended) {
        return Err(Error::Unreachable {
            step: step.id.to_string(),
            login: wait.login.clone(),
            after_s: wait.limit.as_secs(),
            reason: cx.events.mask(ended.last.stderr.join(" ").trim()),
        });
    }
    ended.show_held(cx, step, observe);
    ended.result(cx, step)
}
