//! Vagrant's failures as the runner says them.

use super::{CliError, Error, Failure, tool_error};

/// The runner's error for Vagrant's failed call `e`, made as step `step`: a sign-in the ESXi host
/// refused as `server_auth_failed`, anything else as any tool's.
pub(super) fn vagrant_error(step: &str, e: CliError<Failure>) -> Error {
    match e {
        CliError::Failed {
            failure: Failure::SignInRefused { server },
            ..
        } => Error::ServerAuthFailed {
            step: step.to_owned(),
            server,
            reason: "the ESXi host refused the run's credentials (its esxi_username and password)".to_owned(),
        },
        other => tool_error(step, other),
    }
}
