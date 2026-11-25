//! Compose's failures the runner names by their code: the engine's own.

use std::time::Duration;

use super::{CliError, Error, Failure, ToolCx, tool_error};
use crate::cli::compose;

/// How long the engine is given to answer once a call failed in words that leave its state open.
pub(super) const ASK_ENGINE: Duration = Duration::from_secs(10);

/// `result`'s failure as `to` says it, the engine asked first where Docker's words leave its
/// state open (a failure read as no other, or a pull read as cut short): one that doesn't answer
/// makes it [`Failure::EngineDown`], whatever the words. A failure that names what the engine
/// refused (a taken port, a pool overlap) came from an engine that answered: it isn't asked.
pub(super) async fn checked<T>(cx: &ToolCx<'_>, result: Result<T, CliError<Failure>>, to: impl FnOnce(CliError<Failure>) -> Error) -> crate::Result<T> {
    let e = match result {
        Ok(v) => return Ok(v),
        Err(e) => e,
    };
    let open = match &e {
        CliError::Failed {
            failure: Failure::Other(reason) | Failure::PullTransient(reason),
            ..
        } => Some(reason.clone()),
        _ => None,
    };
    match open {
        Some(reason) if !compose::engine_answers(&cx.read(ASK_ENGINE)).await => Err(to(CliError::Failed {
            tool: "docker",
            failure: Failure::EngineDown(reason),
        })),
        _ => Err(to(e)),
    }
}

/// A Compose call's failure as the runner says it: the engine not answering (stopped, or quit
/// while the step ran) `tool_unanswered`, refusing this account `tool_denied`, a network's address
/// block another holds `address_pool_overlap`, anything else as any tool's.
pub(super) fn engine_error(step: &str, e: CliError<Failure>) -> Error {
    let step = step.to_owned();
    match e {
        CliError::Failed {
            failure: Failure::EngineDown(reason),
            ..
        } => Error::ToolUnanswered {
            step,
            tool: "docker".to_owned(),
            reason,
        },
        CliError::Failed {
            failure: Failure::EngineDenied(reason),
            ..
        } => Error::ToolDenied {
            step,
            tool: "docker".to_owned(),
            reason,
        },
        CliError::Failed {
            failure: Failure::PoolOverlap(reason),
            ..
        } => Error::AddressPoolOverlap { step, reason },
        other => tool_error(&step, other),
    }
}

/// An `up`'s failure as the runner says it: a taken host port is `port_in_use`, with the port and
/// protocol Docker names (0 and TCP when it names none), an image's pull the registry refused
/// `download_failed` (step `pull-<step>`), anything else as any tool's.
pub(super) fn up_error(step: &str, e: CliError<Failure>) -> Error {
    match e {
        CliError::Failed {
            failure: Failure::PortTaken(taken),
            ..
        } => {
            let (port, protocol) = taken.unwrap_or((0, crate::project::platform::Protocol::Tcp));
            Error::PortInUse {
                port: crate::port::HostPort::new(port),
                protocol: crate::encoding::wire_name(&protocol),
                holder: None,
            }
        }
        CliError::Failed {
            failure: Failure::PullRefused(reason),
            ..
        } => Error::DownloadFailed {
            what: crate::error::DownloadKind::Images,
            step: format!("pull-{step}"),
            reason,
        },
        other => engine_error(step, other),
    }
}
