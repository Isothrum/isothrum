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

#[cfg(test)]
mod tests {
    use isothrum_runner::store::EventCursor;
    use isothrum_runner::{ErrorCode, EventKind, Op, ResultData, RunState, Stream};

    use super::*;
    use crate::v1::Event;

    /// Operation `id`'s event of `kind`.
    fn of(id: i64, kind: EventKind) -> Event {
        let mut event = Event::new(1, Some("lab".to_owned()), Some(Op::Up), kind);
        event.operation = Some(OperationId(id));
        event
    }

    fn ended(id: i64) -> Event {
        of(
            id,
            EventKind::Operation {
                phase: OperationPhase::Ended,
                outcome: None,
                position: None,
            },
        )
    }

    /// Operation 7's events read off a subscription that gives `items`, then ends.
    fn following(items: Vec<Result<Event>>) -> OperationEvents {
        let (tx, rx) = tokio::sync::mpsc::channel(16);
        for item in items {
            tx.try_send(item.map(|e| (EventCursor(1), e))).expect("room");
        }
        drop(tx);
        let task = tokio::spawn(async {});
        OperationEvents::new(OperationId(7), Subscription { rx, task })
    }

    #[tokio::test]
    async fn an_operations_events_are_its_own_up_to_its_end_and_none_after() {
        let result = of(
            7,
            EventKind::Result {
                ok: true,
                data: ResultData::Moved { state: RunState::Ready },
            },
        );
        let mut events = following(vec![
            Ok(of(
                8,
                EventKind::Log {
                    step: "up".into(),
                    stream: Stream::Stdout,
                    line: "another's".into(),
                },
            )),
            Ok(result.clone()),
            Ok(ended(7)),
            Ok(of(
                7,
                EventKind::Log {
                    step: "up".into(),
                    stream: Stream::Stdout,
                    line: "after its end".into(),
                },
            )),
        ]);
        let first = events.next().await.expect("an event").expect("its own");
        assert_eq!(first.kind, result.kind, "another operation's event is passed over");
        let last = events.next().await.expect("an event").expect("its own");
        assert_eq!(phase_of(&last), Some(OperationPhase::Ended));
        assert!(events.next().await.is_none(), "nothing after its end, though the stream goes on");
    }

    #[tokio::test]
    async fn an_error_event_is_the_operations_failure_with_its_code_and_step() {
        let failed = of(
            7,
            EventKind::Error {
                code: ErrorCode::ToolFailed,
                message: "compose said no".into(),
                step: Some("up".into()),
                timeout: None,
                detail: None,
            },
        );
        let e = following(vec![Ok(failed), Ok(ended(7))]).outcome().await.expect_err("failed");
        assert!(
            matches!(&e, ClientError::Operation { code: ErrorCode::ToolFailed, step: Some(step), .. } if step == "up"),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn an_operation_that_ended_with_no_outcome_was_cancelled_and_a_broken_stream_is_its_error() {
        let cancelled = following(vec![Ok(ended(7))]).outcome().await.expect_err("no outcome");
        assert_eq!(cancelled.code(), ErrorCode::Cancelled);
        let mut broken = following(vec![Err(ClientError::closed())]);
        let e = broken.next().await.expect("an item").expect_err("the stream's error");
        assert_eq!(e.code(), ErrorCode::Io);
        assert!(broken.next().await.is_none(), "it ended with its error");
    }

    #[tokio::test]
    async fn a_subscription_is_a_stream_of_its_events() {
        use std::pin::Pin;
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.try_send(Ok((EventCursor(4), ended(7)))).expect("room");
        drop(tx);
        let mut sub = Subscription {
            rx,
            task: tokio::spawn(async {}),
        };
        let first = std::future::poll_fn(|cx| futures_core::Stream::poll_next(Pin::new(&mut sub), cx)).await;
        assert!(matches!(first, Some(Ok((EventCursor(4), _)))), "{first:?}");
        assert!(sub.next().await.is_none());
    }
}
