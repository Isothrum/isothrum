//! Vagrant's machine locks. Vagrant lets one process at a time act on a machine, and refuses any
//! other at once (`MachineActionLockedError`) rather than waiting. On Hyper-V even reading a
//! machine's state is such an action (a PowerShell call, a second or two), so another Vagrant
//! process merely reading the run's VMs (a `vagrant global-status --prune`, which reads every
//! VM Vagrant knows, or a `vagrant status` in a terminal) made the run's own status read fail,
//! and the start with it. On every provider, each read of a machine's state also records it in
//! Vagrant's machine index under the machine's index lock, which Vagrant takes without waiting
//! either (`MachineLocked`): two `vagrant status` of the same VMs at once, or one as a halt
//! records the VM off, and one of them fails (seen on libvirt, another process reading the run's
//! status as its start ended). The run's reads and the controller's halt, which a refused call left
//! undone, are tried again while that lock is all that stopped them, for a while. The run's own
//! operation holds them far longer (a `vagrant up` provisioning the VMs): a status meanwhile
//! doesn't read at all ([`Running::status`](crate::Running::status)).

use std::future::Future;
use std::time::Duration;

use isoloom_manifest::Step;

use crate::Result;
use crate::driver::STATUS;
use crate::executor::{Cx, Echo, call, capture_output, stdout_of};
use crate::process::{Captured, ProcessSpec};

/// How long a call refused for another process's lock keeps being tried: a read of the machine
/// holds it for seconds; a process acting on it for longer is no passing read.
pub(super) const PATIENCE: Duration = Duration::from_secs(60);

/// How long between two tries.
const POLL: Duration = Duration::from_secs(2);

/// What Vagrant says when another process holds a machine's lock, its action lock or its index
/// lock: each error's class, which `--machine-readable` prints in its `error-exit` record, and its
/// message, which a human-readable call prints on standard error (the record repeats it too).
const LOCKED: [&str; 4] = [
    "Vagrant::Errors::MachineActionLockedError",
    "another process is already executing an action on the machine",
    "Vagrant::Errors::MachineLocked,",
    "Vagrant can't use the requested machine because it is locked!",
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

/// Reads the run's `lifecycle.status` (`vagrant status --machine-readable`): its standard
/// output, once no other process's lock stops it.
pub(super) async fn read_status(cx: &Cx<'_>) -> Result<String> {
    let step: &Step = &cx.run.manifest().lifecycle.status;
    let out = waiting(STATUS, || capture_output(cx, STATUS, step)).await?;
    stdout_of(cx, step.id.as_str(), None, &out)
}

/// [`call`], tried again while another process's lock is all that refused it.
pub(super) async fn call_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    waiting(id, || call(cx, id, spec, limit, echo)).await
}

/// [`call_retrying`], failing unless it exits 0: its standard output (as
/// [`call_ok`](crate::executor::call_ok)).
pub(super) async fn call_ok_retrying(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<String> {
    let out = call_retrying(cx, id, spec, limit, echo).await?;
    stdout_of(cx, id, Some(spec), &out)
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
        assert!(!refused(&ended(1, &["1,,error-exit,Vagrant::Errors::MachineLockedOut,x"], &[])));
        assert!(!refused(&ended(0, &[read], &[])));
    }

    /// Regression: Vagrant's machine index lock, which every read of a machine's state takes
    /// without waiting, refused a status read on libvirt (`MachineLocked`), and the read wasn't
    /// tried again: neither its class nor its message is `MachineActionLockedError`'s.
    #[test]
    fn a_machines_index_lock_held_by_another_process_is_refused() {
        // `vagrant status --machine-readable`, as Vagrant 2.4.9 printed it on libvirt.
        let said = "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant \
                    process is currently reading or modifying\\nthe machine.";
        let fog = "[fog][WARNING] Unrecognized arguments: libvirt_ip_command";
        let read = format!("1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}");
        assert!(refused(&ended(1, &[&format!("1794342958,,ui,error,{said}"), &read], &[fog])));
        // A `vagrant halt` (human output): the message, on standard error.
        let halt = [
            fog,
            "Vagrant can't use the requested machine because it is locked! This",
            "means that another Vagrant process is currently reading or modifying",
        ];
        assert!(refused(&ended(1, &[], &halt)));
    }
}
