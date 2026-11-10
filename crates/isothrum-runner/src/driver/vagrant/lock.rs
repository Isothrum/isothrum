//! Vagrant's machine locks. Vagrant lets one process at a time act on a machine, and refuses any
//! other at once (`MachineActionLockedError`) rather than waiting. On Hyper-V even reading a
//! machine's state is such an action (a PowerShell call, a second or two), so another Vagrant
//! process merely reading the run's VMs (a `vagrant global-status --prune`, which reads every
//! VM Vagrant knows, or a `vagrant status` in a terminal) made the run's own status read fail,
//! and the start with it. The run's reads and the controller's halt, which a refused call left
//! undone, are tried again while that lock is all that stopped them, for a while.

use std::future::Future;
use std::time::Duration;

use isoloom_manifest::Step;

use crate::Result;
use crate::executor::{Cx, Echo, call, capture_output, step_stdout};
use crate::process::{Captured, ProcessSpec};

/// How long a call refused for another process's lock keeps being tried: a read of the machine
/// holds it for seconds; a process acting on it for longer is no passing read.
pub(super) const PATIENCE: Duration = Duration::from_secs(60);

/// How long between two tries.
const POLL: Duration = Duration::from_secs(2);

/// What Vagrant says when another process holds a machine's lock: the error's class, which
/// `--machine-readable` prints in its `error-exit` record, and its message.
const LOCKED: [&str; 2] = [
    "Vagrant::Errors::MachineActionLockedError",
    "another process is already executing an action on the machine",
];

/// Whether `out`, a failed Vagrant call, was refused because another process held a machine's
/// lock.
pub(super) fn refused(out: &Captured) -> bool {
    !out.succeeded() && out.stdout.iter().chain(&out.stderr).any(|line| LOCKED.iter().any(|said| line.contains(said)))
}

/// `attempt` until it isn't refused for a lock ([`refused`]), [`POLL`] apart, up to [`PATIENCE`]:
/// how the last try ended.
async fn waiting<F, Fut>(what: &str, mut attempt: F) -> Result<Captured>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Captured>>,
{
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let out = attempt().await?;
        if !refused(&out) || tokio::time::Instant::now() + POLL > deadline {
            return Ok(out);
        }
        tracing::debug!(call = what, "another Vagrant process holds a machine's lock: tried again");
        tokio::time::sleep(POLL).await;
    }
}

/// The run's `lifecycle.status` (`vagrant status --machine-readable`): its standard output, once
/// no other process's lock stops it.
pub(super) async fn status(cx: &Cx<'_>) -> Result<String> {
    let step: &Step = &cx.run.manifest().lifecycle.status;
    let out = waiting("lifecycle.status", || capture_output(cx, "lifecycle.status", step)).await?;
    step_stdout(cx, step, &out)
}

/// [`call`], tried again while another process's lock is all that refused it.
pub(super) async fn call_unlocked(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    waiting(id, || call(cx, id, spec, limit, echo)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::Exit;

    fn ended(code: i32, stdout: &[&str], stderr: &[&str]) -> Captured {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Captured {
            exit: Exit::new(Some(code)),
            stdout: lines(stdout),
            stderr: lines(stderr),
        }
    }

    #[test]
    fn only_a_lock_held_by_another_process_is_refused() {
        // `--machine-readable`: the error's class in its `error-exit` record.
        let read = "1,,error-exit,Vagrant::Errors::MachineActionLockedError,An action 'read_state' was attempted";
        assert!(refused(&ended(1, &[read], &[])));
        // Human output: its message, on standard error.
        let halt = "but another process is already executing an action on the machine.";
        assert!(refused(&ended(1, &[], &["An action 'halt' was attempted on the machine 'web',", halt])));
        // Any other failure, and a success that merely prints the words, are not.
        assert!(!refused(&ended(
            1,
            &[],
            &["SSH connection was refused! This usually happens if the VM failed to"]
        )));
        assert!(!refused(&ended(1, &["1,,error-exit,Vagrant::Errors::VBoxManageError,locked"], &[])));
        assert!(!refused(&ended(0, &[read], &[])));
    }
}
