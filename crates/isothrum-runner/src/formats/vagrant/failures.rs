//! Vagrant's failures as the runner says them.

use super::{CliError, Error, Failure, Provider, ToolCx, tool_error};

/// The runner's error for Vagrant's failed call `e`, made as step `step` of the run `cx` names: a
/// sign-in the ESXi host refused as `server_auth_failed`, a hypervisor that found no virtiofsd to
/// share a folder with as `provider_refused` (needing virtiofsd), anything else as any tool's.
pub(super) fn vagrant_error(cx: &ToolCx<'_>, step: &str, e: CliError<Failure>) -> Error {
    match e {
        CliError::Failed {
            failure: Failure::Lacks { need, reason },
            ..
        } => Error::Provider {
            provider: cx.provider.map_or("vagrant", Provider::id).to_owned(),
            reason,
            need: Some(need),
        },
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
