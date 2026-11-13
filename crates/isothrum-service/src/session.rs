//! One client's connection: its `hello`, then its requests, each answered with its id, and the
//! events of its subscription.

use std::sync::Arc;

use interprocess::local_socket::tokio::Stream;
use isothrum_client::proto::{Command, PROTOCOL, Reply, Request};
use isothrum_client::v1::EventCursor;
use isothrum_client::wire::{LineReader, write_line};
use isothrum_runner::{ErrorCode, RunName};
use tokio::sync::broadcast;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Shared;
use crate::bus::{Bus, Numbered};

/// How many replies and events wait for a slow client before its subscription waits too.
const OUTBOX: usize = 1024;

/// Serves the connection `stream` until the client closes it.
pub(crate) async fn serve(stream: Stream, shared: Arc<Shared>) {
    let _client = shared.client_connected();
    let (recv, mut send) = tokio::io::split(stream);
    let mut reader = LineReader::new(recv);
    let (outbox, mut out) = mpsc::channel::<Reply>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(reply) = out.recv().await {
            if let Err(e) = write_line(&mut send, &reply).await {
                tracing::debug!(error = %e, "a client left");
                return;
            }
        }
    });
    let mut session = Session {
        shared,
        outbox,
        subscription: None,
        greeted: false,
    };
    loop {
        match reader.next::<Request>().await {
            Ok(Some(request)) => {
                if !session.handle(request).await {
                    break;
                }
            }
            Ok(None) => break,
            Err(e) => {
                let refused = Reply::Error {
                    id: 0,
                    code: ErrorCode::Usage,
                    message: format!("a line that isn't a request: {e}"),
                };
                drop(session.outbox.send(refused).await);
                break;
            }
        }
    }
    if let Some(subscription) = session.subscription.take() {
        subscription.abort();
    }
    drop(session);
    // What was queued for the client is written before the connection closes.
    drop(writer.await);
}

/// A connection's state.
struct Session {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<Reply>,
    subscription: Option<JoinHandle<()>>,
    greeted: bool,
}

impl Session {
    /// Answers `request`; false when the connection is to end.
    async fn handle(&mut self, request: Request) -> bool {
        let id = request.id;
        if !self.greeted {
            return self.greet(request).await;
        }
        let reply = match request.command {
            Command::Hello { protocol } => hello(id, protocol),
            Command::Status { run, path } => {
                // A status reads the tools: it doesn't hold the connection's other requests.
                let (shared, outbox) = (Arc::clone(&self.shared), self.outbox.clone());
                tokio::spawn(async move {
                    let reply = match shared.engine.status(&run, path.as_deref()).await {
                        Ok(status) => Reply::Status { id, status: Box::new(status) },
                        Err(refusal) => error(id, refusal.code, refusal.message),
                    };
                    drop(outbox.send(reply).await);
                });
                return true;
            }
            Command::List => match self.shared.engine.list() {
                Ok(runs) => Reply::Runs { id, runs },
                Err(refusal) => error(id, refusal.code, refusal.message),
            },
            Command::Operations { run, limit } => Reply::Operations {
                id,
                operations: self.shared.queue.operations(&run, usize::try_from(limit).unwrap_or(usize::MAX)),
            },
            Command::Cancel { operation } => {
                if self.shared.queue.cancel(operation) {
                    Reply::Ok { id }
                } else {
                    error(id, ErrorCode::Usage, format!("no operation {operation} is queued or running"))
                }
            }
            Command::Subscribe { run, after } => return self.subscribe(id, run, after).await,
            Command::Unsubscribe => {
                if let Some(subscription) = self.subscription.take() {
                    subscription.abort();
                }
                Reply::Ok { id }
            }
            Command::Shutdown => {
                self.shared.request_stop();
                Reply::Ok { id }
            }
            command => match self.shared.queue.submit(command) {
                Ok((operation, run, position)) => Reply::Accepted { id, operation, run, position },
                Err(refusal) => error(id, refusal.code, refusal.message),
            },
        };
        self.outbox.send(reply).await.is_ok()
    }

    /// Answers the connection's first request, which must be a `hello` of this protocol.
    async fn greet(&mut self, request: Request) -> bool {
        let reply = match request.command {
            Command::Hello { protocol } => hello(request.id, protocol),
            _ => error(request.id, ErrorCode::Usage, "the first request must be `hello`".to_owned()),
        };
        self.greeted = matches!(reply, Reply::Hello { .. });
        self.outbox.send(reply).await.is_ok() && self.greeted
    }

    /// Starts the connection's subscription (replacing one it had), registered before its `ok`
    /// is sent, and the `ok` sent before its first event: no event after it is missed. False
    /// when the client left.
    async fn subscribe(&mut self, id: u64, run: Option<RunName>, after: Option<EventCursor>) -> bool {
        if let Some(old) = self.subscription.take() {
            old.abort();
        }
        let (replay, live) = self.shared.bus.subscribe(after);
        if self.outbox.send(Reply::Ok { id }).await.is_err() {
            return false;
        }
        let (bus, outbox) = (Arc::clone(&self.shared.bus), self.outbox.clone());
        self.subscription = Some(tokio::spawn(forward(bus, run, replay, live, outbox)));
        true
    }
}

/// The reply to a `hello` of `protocol`.
fn hello(id: u64, protocol: u32) -> Reply {
    if protocol == PROTOCOL {
        Reply::Hello {
            id,
            protocol: PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    } else {
        error(id, ErrorCode::Usage, format!("this service speaks protocol {PROTOCOL}, the client {protocol}"))
    }
}

/// An error reply.
fn error(id: u64, code: ErrorCode, message: String) -> Reply {
    Reply::Error { id, code, message }
}

/// Sends a subscription's events to `outbox`: the replayed ones, then the live ones, those of
/// `run` only when it names one. A subscriber that fell behind the live events resumes from the
/// kept ones after the last it got.
async fn forward(bus: Arc<Bus>, run: Option<RunName>, replay: Vec<Numbered>, mut live: broadcast::Receiver<Numbered>, outbox: mpsc::Sender<Reply>) {
    let mut last: Option<EventCursor> = None;
    let mut pending = replay;
    loop {
        for (cursor, event) in pending.drain(..) {
            if last.is_some_and(|l| cursor <= l) {
                continue;
            }
            last = Some(cursor);
            if run.as_ref().is_some_and(|r| event.run.as_deref() != Some(r.as_str())) {
                continue;
            }
            let frame = Reply::Event {
                cursor,
                event: Box::new((*event).clone()),
            };
            if outbox.send(frame).await.is_err() {
                return;
            }
        }
        match live.recv().await {
            Ok(numbered) => pending.push(numbered),
            Err(RecvError::Lagged(missed)) => {
                tracing::debug!(missed, "a subscriber fell behind: resuming from the kept events");
                let (kept, resumed) = bus.subscribe(last);
                pending = kept;
                live = resumed;
            }
            Err(RecvError::Closed) => return,
        }
    }
}
