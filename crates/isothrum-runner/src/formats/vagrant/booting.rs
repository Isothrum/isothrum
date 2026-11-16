//! A machine still booting: an `up` that provisions it before its SSH server answers is made
//! again once it may.

use std::time::Duration;

use super::{CliError, Failure, ToolCx, WarningCode};

/// How long an `up` waits before it is made again, a machine still booting.
const PAUSE: Duration = Duration::from_secs(15);

/// How many times it is made again: the machine has two minutes to answer.
const TRIES: usize = 8;

/// `up`, made again while a machine it provisions still boots ([`Failure::Booting`]: its SSH
/// server doesn't answer yet), [`PAUSE`] apart, up to [`TRIES`] times, said once as a `retrying`
/// warning. Vagrant waits for a machine it boots itself; one already running (booted by a start
/// that was cut off, which the next `up` takes over) is provisioned at once, booted or not. A
/// failure past the last try is the call's.
pub(super) async fn until_it_answers<F, Fut>(cx: &ToolCx<'_>, step: &str, mut up: F) -> Result<(), CliError<Failure>>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), CliError<Failure>>>,
{
    let mut tries = 0;
    loop {
        match up().await {
            Err(CliError::Failed {
                failure: Failure::Booting { .. },
                ..
            }) if tries < TRIES => {
                if tries == 0 {
                    (cx.warn)(
                        WarningCode::Retrying,
                        &format!("`vagrant {step}`: a machine still boots and its SSH server doesn't answer yet; running it again"),
                    );
                }
                tries += 1;
                tokio::time::sleep(PAUSE).await;
            }
            done => return done,
        }
    }
}
