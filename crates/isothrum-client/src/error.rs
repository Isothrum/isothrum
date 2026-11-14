//! Why a client call failed.

use isothrum_runner::ErrorCode;

/// Why a client call failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// No service answered for the home, and none could be started.
    #[error("no runner service for this home: {0}")]
    Unavailable(String),
    /// A frame the other side can't read, or a protocol the service doesn't speak.
    #[error("protocol: {0}")]
    Protocol(String),
    /// The service refused the request.
    #[error("{message}")]
    Refused {
        /// Why.
        code: ErrorCode,
        /// What it said.
        message: String,
    },
    /// The operation failed (its `error` event), or was cancelled.
    #[error("{message}")]
    Operation {
        /// Why.
        code: ErrorCode,
        /// What it said.
        message: String,
        /// The step that failed, when one did.
        step: Option<String>,
    },
    /// Reading or writing the connection failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl ClientError {
    /// Its code, as an `error` event gives it.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            ClientError::Refused { code, .. } | ClientError::Operation { code, .. } => *code,
            ClientError::Unavailable(_) | ClientError::Protocol(_) | ClientError::Io(_) => ErrorCode::Io,
        }
    }

    /// The connection ended under a call.
    pub(crate) fn closed() -> ClientError {
        ClientError::Unavailable("the service closed the connection".to_owned())
    }
}
