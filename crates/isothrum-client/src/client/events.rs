//! A subscription's events, and one operation's read off it to its end.

use super::{
    Accepted, ClientError, Context, ErrorCode, Event, EventCursor, EventKind, OperationId, OperationPhase, Pin, Poll, Result, is_outcome, mpsc, phase_of,
};

/// A subscription: the events of the stream, each with its cursor. It ends when the service
/// closes the connection (an error item says so); dropping it unsubscribes.
pub struct Subscription {
    pub(super) rx: mpsc::Receiver<Result<(EventCursor, Event)>>,
    pub(super) task: tokio::task::JoinHandle<()>,
}

impl Subscription {
    /// The next event; `None` once the subscription ended.
    pub async fn next(&mut self) -> Option<Result<(EventCursor, Event)>> {
        self.rx.recv().await
    }
}

impl futures_core::Stream for Subscription {
    type Item = Result<(EventCursor, Event)>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription").finish_non_exhaustive()
    }
}

/// An operation queued and followed ([`Client::follow`](crate::Client::follow)).
#[derive(Debug)]
pub struct Followed {
    /// The service's acceptance.
    pub accepted: Accepted,
    pub(super) events: OperationEvents,
}

impl Followed {
    /// The operation's next event; `None` after its `ended`.
    pub async fn next(&mut self) -> Option<Result<Event>> {
        self.events.next().await
    }

    /// Reads the operation to its end, and returns its `result`, else its `error` as
    /// [`ClientError::Operation`].
    ///
    /// # Errors
    ///
    /// It failed, was cancelled, or the connection ended.
    pub async fn outcome(&mut self) -> Result<Event> {
        self.events.outcome().await
    }
}

/// One operation's events, read off a subscription.
#[derive(Debug)]
pub(super) struct OperationEvents {
    operation: OperationId,
    events: Subscription,
    ended: bool,
}

impl OperationEvents {
    pub(super) fn new(operation: OperationId, events: Subscription) -> OperationEvents {
        OperationEvents {
            operation,
            events,
            ended: false,
        }
    }

    pub(super) async fn next(&mut self) -> Option<Result<Event>> {
        if self.ended {
            return None;
        }
        loop {
            let event = match self.events.next().await? {
                Ok((_, event)) => event,
                Err(e) => {
                    self.ended = true;
                    return Some(Err(e));
                }
            };
            if event.operation != Some(self.operation) {
                continue;
            }
            if phase_of(&event) == Some(OperationPhase::Ended) {
                self.ended = true;
            }
            return Some(Ok(event));
        }
    }

    pub(super) async fn outcome(&mut self) -> Result<Event> {
        let mut outcome = None;
        while let Some(event) = self.next().await {
            let event = event?;
            if is_outcome(&event) {
                outcome = Some(event);
            }
        }
        outcome_of(outcome)
    }
}

/// An operation's outcome from its last `result` or `error` event (none: it was cancelled before
/// it began).
pub(super) fn outcome_of(event: Option<Event>) -> Result<Event> {
    let Some(event) = event else {
        return Err(ClientError::Operation {
            code: ErrorCode::Cancelled,
            message: "the operation was cancelled before it began".to_owned(),
            step: None,
            detail: None,
        });
    };
    match &event.kind {
        EventKind::Error {
            code, message, step, detail, ..
        } => Err(ClientError::Operation {
            code: *code,
            message: message.clone(),
            step: step.clone(),
            detail: detail.clone().map(Box::new),
        }),
        _ => Ok(event),
    }
}
