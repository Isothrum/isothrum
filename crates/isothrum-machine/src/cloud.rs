//! The cloud accounts this computer's cloud CLIs are signed in to, asked through those CLIs:
//! whether a sign-in still reaches its provider ([`Failure`] with [`Refusal::CredentialsExpired`]
//! when it ran out or is missing), who it is, and what the account offers: AWS's profiles, its
//! spend this month and whether it may create instances ([`aws`]), Azure's subscriptions, the VM
//! sizes a region offers a subscription and its core quotas ([`azure`]), Google Cloud's billing
//! accounts and organizations ([`gcp`]), and Oracle Cloud's compartments and config file
//! ([`oci`]).
//!
//! Each CLI is one adapter here: its argument lists, its runs (through the context's runner, each
//! within [`CLI_TIMEOUT`]) and the reading of its answers, a refusal read from its words (each
//! wording backed by a fixture of the CLI's, under `cloud/fixtures/<cli>/`). What the answers mean
//! to a user, and how a refusal is put to them, is the caller's.

use std::time::Duration;

use isothrum_runner::ProcessSpec;

use crate::context::Context;
use crate::failure::{self, Failure, Refusal};

pub mod aws;
pub mod azure;
pub mod gcp;
pub mod oci;

pub use isothrum_runner::Cloud;

/// How long a cloud CLI's answer may take: a call over the network to the provider, slower than a
/// local probe.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(60);

/// The CLI a cloud's account is signed in with on this computer: `aws`, `az`, `gcloud`, `oci`.
/// None for a cloud whose credential is an API token alone (DigitalOcean, Linode).
#[must_use]
pub fn cli(cloud: Cloud) -> Option<&'static str> {
    match cloud {
        Cloud::Aws => Some(aws::CLI),
        Cloud::Azure => Some(azure::CLI),
        Cloud::Gcp => Some(gcp::CLI),
        Cloud::Oci => Some(oci::CLI),
        _ => None,
    }
}

/// `spec` within `limit`: its standard output, else why not (a refusal read by `read`).
async fn run(cx: &Context, limit: Duration, spec: &ProcessSpec, read: fn(&str) -> Refusal) -> Result<String, Failure> {
    failure::answer(cx, limit, spec, read).await
}

/// `program args` within [`CLI_TIMEOUT`].
async fn ask(cx: &Context, program: &str, args: &[&str], read: fn(&str) -> Refusal) -> Result<String, Failure> {
    run(cx, CLI_TIMEOUT, &ProcessSpec::new(program, args.iter().copied()), read).await
}

/// An answer that isn't what `program` was asked for, said as [`Failure::Unreadable`].
fn unreadable(program: &str, why: impl std::fmt::Display) -> Failure {
    Failure::Unreadable {
        program: program.to_owned(),
        why: why.to_string(),
    }
}

/// A JSON list a CLI answered, each entry as it is: an answer that isn't a list is unreadable
/// (a changed format, or a warning printed instead).
fn json_list(program: &str, out: &str) -> Result<Vec<serde_json::Value>, Failure> {
    serde_json::from_str(out).map_err(|e| unreadable(program, format!("its answer isn't a JSON list ({e})")))
}

/// A one-line answer, trimmed.
fn line(out: &str) -> String {
    out.trim().to_owned()
}

/// Whether `said` holds any of `words`, in any case.
fn says(said: &str, words: &[&str]) -> bool {
    let lower = said.to_ascii_lowercase();
    words.iter().any(|w| lower.contains(&w.to_ascii_lowercase()))
}

/// A browser sign-in to a cloud through its CLI ([`sign_in`]).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignIn {
    /// `aws login` (AWS CLI 2.32 or later): console credentials for up to 12 hours, for the
    /// CLI's default profile or `profile`.
    Aws {
        /// The profile signed in, when not the default.
        profile: Option<String>,
    },
    /// `az login`.
    Azure,
    /// `gcloud auth login --update-adc`: the CLI's own sign-in and the application-default
    /// credentials (what Terraform's Google provider reads) in one browser flow.
    Gcp,
}

impl SignIn {
    /// The CLI run, and its arguments.
    #[must_use]
    pub fn command(&self) -> ProcessSpec {
        match self {
            SignIn::Aws { profile } => {
                let mut args = vec!["login".to_owned()];
                if let Some(p) = profile {
                    args.extend(["--profile".to_owned(), p.clone()]);
                }
                ProcessSpec::new(aws::CLI, args)
            }
            SignIn::Azure => ProcessSpec::new(azure::CLI, ["login"]),
            SignIn::Gcp => ProcessSpec::new(gcp::CLI, ["auth", "login", "--update-adc"]),
        }
    }
}

/// Signs in to a cloud in the browser through its CLI ([`SignIn`]): `on_line` gets the command
/// (`$ az login`), then each line the CLI prints, until it ends. No time limit: the browser waits
/// for its user; dropping the future ends the CLI (the runner's), which is how a caller cancels it
/// or gives it a limit.
///
/// # Errors
///
/// [`Failure`]: the CLI isn't installed or couldn't be started, or it failed (what it said last).
pub async fn sign_in(cx: &Context, which: &SignIn, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<(), Failure> {
    let spec = which.command();
    on_line(&format!("$ {}", spec.line()));
    let mut last = String::new();
    let mut each = |_: isothrum_runner::Stream, l: &str| {
        if !l.trim().is_empty() {
            l.trim().clone_into(&mut last);
        }
        on_line(l);
    };
    let program = spec.program.clone();
    match cx.runner.run(&spec, &mut each).await {
        Ok(exit) if exit.success() => Ok(()),
        Ok(exit) => Err(Failure::Refused {
            refusal: Refusal::Other,
            said: if last.is_empty() { exit.to_string() } else { last },
            program,
        }),
        Err(isothrum_runner::ProcessError::Missing { .. }) => Err(Failure::Missing { program }),
        Err(other) => Err(Failure::NotStarted {
            program,
            reason: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests;
