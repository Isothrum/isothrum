//! Compose's failures the runner names by their code: the engine's own.

use super::{CliError, Error, Failure, tool_error};

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
