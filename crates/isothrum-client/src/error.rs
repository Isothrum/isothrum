//! Why a client call failed.

use isothrum_runner::ErrorCode;
use isothrum_runner::store::OperationId;

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
    /// The home's service is another release and has operations queued or running: it was left
    /// running ([`Client::connect_exact`](crate::Client::connect_exact)).
    #[error("the runner service (Isothrum {version}, pid {pid}) has {} operation(s) queued or running: it was left running", operations.len())]
    Busy {
        /// The service's release.
        version: String,
        /// Its process id.
        pid: u32,
        /// Its operations queued or running, oldest first.
        operations: Vec<OperationId>,
    },
    /// The service started is another release than this client.
    #[error("the runner service is Isothrum {service}, this client {client}")]
    Version {
        /// This client's release ([`VERSION`](crate::VERSION)).
        client: String,
        /// The service's.
        service: String,
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
            ClientError::Busy { .. } => ErrorCode::Locked,
            ClientError::Version { .. } => ErrorCode::Unsupported,
            ClientError::Unavailable(_) | ClientError::Protocol(_) | ClientError::Io(_) => ErrorCode::Io,
        }
    }

    /// The connection ended under a call.
    pub(crate) fn closed() -> ClientError {
        ClientError::Unavailable("the service closed the connection".to_owned())
    }
}
