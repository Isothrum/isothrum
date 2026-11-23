//! Vagrant's machine locks. Vagrant lets one process at a time act on a machine and refuses any
//! other at once (`MachineActionLockedError`), and every read of a machine's state takes its index
//! lock without waiting (`MachineLocked`): another process merely reading the run's VMs (a
//! `vagrant global-status`, a `vagrant status` in a terminal) makes the run's own call fail.
//!
//! A call refused so is made again, [`POLL`] apart, for up to its patience after the first
//! refusal, when a second run does nothing twice ([`Rerun`]): a read, and a call that converges
//! (`destroy`, `halt`, `suspend`, an `up` not told to provision) always; one that repeats
//! (`provision`, `resume`, an `up` told to provision) only when the refused try had announced no
//! action on a machine (the adapter's `Locked { acted }`): a refusal doesn't always come before
//! Vagrant acted (the index lock is taken at every state read, in the middle of an action too).

use std::future::Future;
use std::time::Duration;

use crate::cli::CliError;
use crate::cli::vagrant::Failure;
use crate::events::WarningCode;

/// How long, after the first refusal, a call refused for another process's lock keeps being
/// made: a read of the machine holds it for seconds; a process acting on it for longer is no
/// passing read.
pub(super) const PATIENCE: Duration = Duration::from_secs(60);

/// How long a status read keeps being made after its first refusal: a passing read of another
/// process holds a machine for seconds, and whoever asks for the status waits meanwhile.
pub(super) const STATUS_PATIENCE: Duration = Duration::from_secs(10);

/// How long between two tries.
const POLL: Duration = Duration::from_secs(2);

/// What a call does when it is made a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Rerun {
    /// Reads, acts on no machine: made again without a word.
    Read,
    /// Acts only on what the first run left.
    Converges,
    /// Provisions or restarts a machine the first run already did.
    Repeats,
}

impl Rerun {
    /// Whether a try refused for a lock may be made again (`acted`: the refused try had announced
    /// an action on a machine).
    fn again(self, acted: bool) -> bool {
        match self {
            Rerun::Read | Rerun::Converges => true,
            Rerun::Repeats => !acted,
        }
    }
}

/// `attempt` until another process's lock is no longer all that refuses it, as long as `rerun`
/// allows another try and `patience` after the first refusal isn't spent: how the last try ended.
/// Each new try of a call that acts is said through `warn` (`retrying`), naming `what`.
pub(super) async fn retrying<T, F, Fut>(
    warn: &(dyn Fn(WarningCode, &str) + Send + Sync),
    what: &str,
    rerun: Rerun,
    patience: Duration,
    mut attempt: F,
) -> Result<T, CliError<Failure>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, CliError<Failure>>>,
{
    let mut deadline = None;
    loop {
        let ended = attempt().await;
        let again = match &ended {
            Err(CliError::Failed {
                failure: Failure::Locked { acted, .. },
                ..
            }) => rerun.again(*acted),
            _ => false,
        };
        if !again {
            return ended;
        }
        let now = tokio::time::Instant::now();
        let until = *deadline.get_or_insert(now + patience);
        if now + POLL > until {
            return ended;
        }
        tokio::time::sleep(POLL).await;
        tracing::debug!(call = what, "another Vagrant process holds a machine's lock: made again");
        if rerun != Rerun::Read {
            warn(
                WarningCode::Retrying,
                &format!("`vagrant {what}`: another Vagrant process held a machine's lock; running it again"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn locked(acted: bool) -> CliError<Failure> {
        CliError::Failed {
            tool: "vagrant",
            failure: Failure::Locked { acted, machines: Vec::new() },
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_call_that_converges_is_made_again_until_it_goes_through() {
        let tries = AtomicUsize::new(0);
        let warned = Mutex::new(Vec::new());
        let warn = |code: WarningCode, said: &str| warned.lock().unwrap().push((code, said.to_owned()));
        let out = retrying(&warn, "halt", Rerun::Converges, PATIENCE, || async {
            if tries.fetch_add(1, Ordering::SeqCst) < 2 { Err(locked(true)) } else { Ok(7) }
        })
        .await;
        assert_eq!(out.unwrap(), 7);
        assert_eq!(tries.load(Ordering::SeqCst), 3);
        assert_eq!(warned.lock().unwrap().len(), 2, "each new try is said");
    }

    #[tokio::test(start_paused = true)]
    async fn a_call_that_repeats_is_made_again_only_when_it_hadnt_acted() {
        let quiet = |_: WarningCode, _: &str| {};
        let tries = AtomicUsize::new(0);
        let out: Result<(), _> = retrying(&quiet, "provision", Rerun::Repeats, PATIENCE, || async {
            tries.fetch_add(1, Ordering::SeqCst);
            Err(locked(true))
        })
        .await;
        assert!(
            matches!(
                out,
                Err(CliError::Failed {
                    failure: Failure::Locked { acted: true, .. },
                    ..
                })
            ),
            "{out:?}"
        );
        assert_eq!(tries.load(Ordering::SeqCst), 1, "it had acted: never twice");
        let fresh = AtomicUsize::new(0);
        let through: Result<(), _> = retrying(&quiet, "provision", Rerun::Repeats, PATIENCE, || async {
            if fresh.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(locked(false))
            } else {
                Ok(())
            }
        })
        .await;
        through.expect("a call that hadn't acted is made again");
        assert_eq!(fresh.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn the_patience_ends_the_tries_and_other_failures_end_them_at_once() {
        let quiet = |_: WarningCode, _: &str| {};
        let tries = AtomicUsize::new(0);
        let out: Result<(), _> = retrying(&quiet, "status", Rerun::Read, STATUS_PATIENCE, || async {
            tries.fetch_add(1, Ordering::SeqCst);
            Err(locked(false))
        })
        .await;
        assert!(matches!(
            out,
            Err(CliError::Failed {
                failure: Failure::Locked { .. },
                ..
            })
        ));
        assert_eq!(tries.load(Ordering::SeqCst), 6, "10 s of patience, 2 s apart, after the first");
        let other = AtomicUsize::new(0);
        let failed: Result<(), _> = retrying(&quiet, "up", Rerun::Converges, PATIENCE, || async {
            other.fetch_add(1, Ordering::SeqCst);
            Err(CliError::Failed {
                tool: "vagrant",
                failure: Failure::Other("boom".into()),
            })
        })
        .await;
        assert!(
            matches!(&failed, Err(CliError::Failed { failure: Failure::Other(why), .. }) if why == "boom"),
            "{failed:?}"
        );
        assert_eq!(other.load(Ordering::SeqCst), 1, "never tried again");
    }
}
