//! What a machine-setup operation couldn't do. One error type for the whole crate so a Tauri
//! command can turn any failure into a single message for the setup screen.

use std::time::Duration;

/// A machine-setup failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A tool couldn't be started (not installed, or an OS error).
    #[error(transparent)]
    Process(#[from] isothrum_runner::process::ProcessError),
    /// A tool the caller needed to succeed exited non-zero.
    #[error("`{command}` failed: {reason}")]
    Exit {
        /// The command line that failed.
        command: String,
        /// Its last line of output, or its exit code.
        reason: String,
    },
    /// An operation ran past its time limit.
    #[error("timed out after {limit:?} waiting for {what}")]
    Timeout {
        /// What was being waited for.
        what: String,
        /// The limit it passed.
        limit: Duration,
    },
    /// The request or the environment was wrong, with a plain-language reason.
    #[error("{0}")]
    Invalid(String),
}

/// The result of a machine-setup operation.
pub type Result<T> = std::result::Result<T, Error>;
