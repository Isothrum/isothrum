//! Why a client call failed.

use isothrum_runner::store::OperationId;
use isothrum_runner::{ErrorCode, ErrorReport};

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
    /// The service refused the request: the runner's report of why (boxed: it is large, and
    /// rare).
    #[error("{0}")]
    Refused(Box<ErrorReport>),
    /// The operation failed (its `error` event's report), or was cancelled.
    #[error("{0}")]
    Operation(Box<ErrorReport>),
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
    /// The request was sent, but the connection ended before the service answered it: whether
    /// the service acted on it is unknown (an operation may have been queued).
    #[error("the runner service's connection ended before it answered")]
    Unanswered,
}

impl ClientError {
    /// Whether it says the connection is gone (the service was killed or restarted, the
    /// connection broke, none answers), rather than an answer of the service's: a call worth one
    /// more try on a new connection ([`Pool::call`](crate::Pool::call)). An operation sent but
    /// unanswered ([`ClientError::Unanswered`]) may have been queued: it is followed again
    /// ([`Pool::follow`](crate::Pool::follow)), not sent again.
    #[must_use]
    pub fn is_broken(&self) -> bool {
        matches!(
            self,
            ClientError::Io(_) | ClientError::Protocol(_) | ClientError::Unavailable(_) | ClientError::Unanswered
        )
    }

    /// Its code, as an `error` event gives it.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            ClientError::Refused(report) | ClientError::Operation(report) => report.code,
            ClientError::Busy { .. } => ErrorCode::Locked,
            ClientError::Version { .. } => ErrorCode::Unsupported,
            ClientError::Unavailable(_) | ClientError::Protocol(_) | ClientError::Io(_) | ClientError::Unanswered => ErrorCode::Io,
        }
    }

    /// The runner's report of the failure, when the service or the operation gave one.
    #[must_use]
    pub fn report(&self) -> Option<&ErrorReport> {
        match self {
            ClientError::Refused(report) | ClientError::Operation(report) => Some(report),
            _ => None,
        }
    }

    /// The connection ended under a call.
    pub(crate) fn closed() -> ClientError {
        ClientError::Unavailable("the service closed the connection".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_failure_has_the_code_an_error_event_would() {
        let refused = ClientError::Refused(Box::new(ErrorReport::new(ErrorCode::NotRunning, "x")));
        assert_eq!(refused.code(), ErrorCode::NotRunning);
        let busy = ClientError::Busy {
            version: "2.1.7".into(),
            pid: 1,
            operations: Vec::new(),
        };
        assert_eq!(busy.code(), ErrorCode::Locked);
        let version = ClientError::Version {
            client: "3.0.0".into(),
            service: "2.1.7".into(),
        };
        assert_eq!(version.code(), ErrorCode::Unsupported);
        for io in [
            ClientError::Protocol("x".into()),
            ClientError::Unavailable("x".into()),
            ClientError::closed(),
            ClientError::Unanswered,
        ] {
            assert_eq!(io.code(), ErrorCode::Io, "{io}");
        }
        assert!(matches!(ClientError::closed(), ClientError::Unavailable(m) if m == "the service closed the connection"));
    }
}
