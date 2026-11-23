//! Failures that pass: an `up` they cut short is made once more, said as a `retrying` warning.
//! `up` brings the run to its files from wherever the cut left it.
//!
//! - UTM: an Apple event failed in passing ([`Failure::ScriptingPassing`]: UTM answering
//!   "Application isn't running" of itself while a VM boots or right after an import, or timing
//!   out while busy), a VM it had shut down for new settings included.
//! - VMware desktop: a host-only network made but said not enabled ([`Failure::VmnetNotEnabled`]).
//!   On a Mac whose Fusion runs on macOS's own networking, its `vmnet-cli --start` exits 1 whatever
//!   happens (its old host-only adapters always read "disabled"), so the Vagrant VMware Utility
//!   reports every new host-only network as "Failed to enable device", though it wrote the network
//!   into Fusion's configuration. The second `up` finds that network and uses it: every first
//!   private network of a subnet on a Mac failed so, once.
//!
//! A second such failure is the call's.

use super::{CliError, Failure, Provider, ToolCx, WarningCode};

/// Why `failure` on `provider` passes, as the `retrying` warning words it; `None` when it doesn't.
fn passes(failure: &Failure, provider: Option<Provider>) -> Option<String> {
    match (failure, provider) {
        (Failure::ScriptingPassing { number, .. }, Some(Provider::Utm)) => {
            Some(format!("UTM failed an Apple event in passing ({number}), as it does while a VM boots"))
        }
        (Failure::VmnetNotEnabled { .. }, Some(Provider::VmwareDesktop)) => Some(
            "VMware made the private network's host-only device but said it couldn't enable it, as Fusion on current macOS does; the device is there now"
                .to_owned(),
        ),
        _ => None,
    }
}

/// `up` (a start, a resume, an update's `up --provision`), made once more when a failure that
/// passes on the run's provider cut it short ([`passes`]).
pub(super) async fn once_more<F, Fut>(cx: &ToolCx<'_>, step: &str, mut up: F) -> Result<(), CliError<Failure>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), CliError<Failure>>>,
{
    let done = up().await;
    let why = match &done {
        Err(CliError::Failed { failure, .. }) => passes(failure, cx.provider),
        _ => None,
    };
    let Some(why) = why else { return done };
    (cx.warn)(WarningCode::Retrying, &format!("`vagrant {step}`: {why}; running it again"));
    up().await
}
