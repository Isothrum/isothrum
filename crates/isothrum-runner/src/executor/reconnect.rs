//! An `ssh.run` step with a `wait_s`: its host may still be booting (a cloud VM right after the
//! apply that made it), so a run that ssh itself couldn't connect for (exit 255, nothing printed
//! by the command: [`ssh::unreachable`]) is tried again every [`ssh::RETRY`] until `wait_s` is
//! spent ([`ssh::Retrying`], as the guest delivery's), then fails with `unreachable`. Only
//! the connection is retried: a command that ran and failed fails the step at once. ssh's own
//! complaints are held back while it can't connect, so the step's log is its command's.

use isoloom_manifest::Step;

use super::Cx;
use super::step::{Ended, Hold, once};
use crate::Result;
use crate::events::WarningCode;
use crate::ssh::{self, SshWait};

impl ssh::Try for Ended {
    fn unreachable(&self) -> bool {
        ssh::unreachable(self.last.exit, self.printed)
    }

    fn said(&self) -> String {
        self.last.said()
    }
}

/// Runs `step` until its host answers within `wait`, then as any step.
pub(crate) async fn run(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send), wait: &SshWait) -> Result<()> {
    let note = format!("{} doesn't answer over SSH yet; trying again for up to {}s", wait.login, wait.limit.as_secs());
    let mut retrying = ssh::Retrying::new(wait);
    let ended = loop {
        let ended = once(cx, list, step, observe, Hold::UntilOutput).await?;
        if !retrying
            .again(ssh::Try::unreachable(&ended), || cx.events.warning(WarningCode::Retrying, &note))
            .await
        {
            break ended;
        }
    };
    let mut ended = retrying.answered(cx, step.id.as_str(), ended)?;
    ended.show_held(cx, step, observe);
    ended.result(cx, step)
}
