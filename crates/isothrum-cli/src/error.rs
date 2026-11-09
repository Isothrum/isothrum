//! Why the command line stopped: one enum for the binary. Its message is the line printed after
//! `error: `; the runner's errors already say their cause.

use isothrum_runner::ErrorCode;

use crate::cancel::Signal;

/// The binary's result.
pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// Why a command failed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The command line doesn't make sense (a run with no manifest, a value missing).
    #[error("{0}")]
    Usage(String),
    /// The runner refused or failed.
    #[error(transparent)]
    Runner(#[from] isothrum_runner::Error),
    /// A folder the command reads couldn't be read.
    #[error("{} couldn't be read: {source}", path.display())]
    Read {
        /// The folder.
        path: std::path::PathBuf,
        /// The OS's reason.
        #[source]
        source: std::io::Error,
    },
    /// Writing the output failed (not because its reader left: that ends nothing).
    #[error("writing the output failed: {0}")]
    Output(#[source] std::io::Error),
    /// A run's status or event couldn't be written as JSON.
    #[error("writing JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A signal asked the command to stop; the operation in flight was dropped, and the tools it
    /// started stopped with it.
    #[error("cancelled by {}: the tools the command started were stopped", .0.name())]
    Cancelled(Signal),
}

impl Error {
    /// The code an `error` event gives it: the runner's own, else the closest one.
    pub(crate) fn code(&self) -> ErrorCode {
        match self {
            Error::Usage(_) => ErrorCode::Usage,
            Error::Runner(e) => e.code(),
            Error::Read { .. } | Error::Output(_) | Error::Json(_) => ErrorCode::Io,
            Error::Cancelled(_) => ErrorCode::Cancelled,
        }
    }
}
