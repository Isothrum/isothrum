//! UTM's passing Apple-event failures: an `up` they cut short is made once more.

use super::{CliError, Failure, Provider, ToolCx, WarningCode};

/// `up` (a start, a resume, an update's `up --provision`), made once more on UTM when an Apple
/// event cut it short in passing ([`Failure::ScriptingPassing`]: UTM answering "Application isn't
/// running" of itself while a VM boots or right after an import, or timing out while busy), said
/// as a `retrying` warning: `up` brings the run to its files from wherever the cut left it, a VM it
/// had shut down for new settings included. A second such failure is the call's.
pub(super) async fn once_more<F, Fut>(cx: &ToolCx<'_>, step: &str, mut up: F) -> Result<(), CliError<Failure>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), CliError<Failure>>>,
{
    match up().await {
        Err(CliError::Failed {
            failure: Failure::ScriptingPassing { number, .. },
            ..
        }) if cx.provider == Some(Provider::Utm) => {
            (cx.warn)(
                WarningCode::Retrying,
                &format!("`vagrant {step}`: UTM failed an Apple event in passing ({number}), as it does while a VM boots; running it again"),
            );
            up().await
        }
        done => done,
    }
}
