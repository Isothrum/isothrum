//! One client's connection: its `hello`, then its requests, each answered with its id, and the
//! events of its subscription.

use std::sync::Arc;

use interprocess::local_socket::tokio::Stream;
use isothrum_client::proto::{Command, PROTOCOL, ReachedConsole, Reply, Request};
use isothrum_client::v1::EventCursor;
use isothrum_client::wire::{LineReader, write_line};
use isothrum_runner::{ErrorCode, RunName};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::Shared;
use crate::engine::{Opened, Refusal};
use crate::feed::Feed;

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
        greeted: Greeted::Not,
        console: None,
    };
    // The service stopped: no request is read any more, what was answered is still written.
    let mut closed = session.shared.feed.closed();
    while let Some(request) = session.next_request(&mut reader, &mut closed).await {
        if !session.handle(request).await {
            break;
        }
    }
    if let Some(subscription) = session.subscription.take() {
        // Once the service stopped, a subscription sends what is kept (its operations' last
        // events), then ends; a client that left has it stopped at once.
        if *closed.borrow() {
            drop(subscription.await);
        } else {
            subscription.abort();
        }
    }
    drop(session);
    // What was queued for the client is written before the connection closes.
    drop(writer.await);
}

/// What a connection's loop meets next.
enum Next {
    /// A request read (or none: the client closed; or a line that isn't one).
    Read(std::io::Result<Option<Request>>),
    /// The service stopped.
    Stopped,
    /// The tunnel of the connection's console ended on its own (its request's id, and how).
    TunnelEnded(u64, String),
}

/// A connection's state.
struct Session {
    shared: Arc<Shared>,
    outbox: mpsc::Sender<Reply>,
    subscription: Option<JoinHandle<()>>,
    greeted: Greeted,
    /// The tunnel of the graphical console this connection asked for (its request's id): open
    /// while the connection is.
    console: Option<(u64, isothrum_runner::Tunnel)>,
}

/// Whether the client said `hello`, and in which protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Greeted {
    /// Not yet: only `hello` is taken.
    Not,
    /// In this one: every request is taken.
    Same,
    /// In another: only what every protocol keeps (`shutdown_if_idle`) is taken.
    Other,
}

impl Session {
    /// Answers `request`; false when the connection is to end.
    async fn handle(&mut self, request: Request) -> bool {
        let id = request.id;
        if self.greeted == Greeted::Not {
            return self.greet(request).await;
        }
        if let Some(refused) = self.refused_as_other(&request) {
            return self.outbox.send(refused).await.is_ok();
        }
        let reply = match request.command {
            Command::Hello { .. } => hello(id),
            command if command.is_question() => match self.ask(id, command).await {
                Some(reply) => reply,
                None => return true,
            },
            Command::Variables { target } => match self.shared.engine.variables(&target) {
                Ok(variables) => Reply::Variables { id, variables },
                Err(refusal) => refused(id, refusal),
            },
            Command::List => match runs(&self.shared.store) {
                Ok(runs) => Reply::Runs { id, runs },
                Err(e) => refused(id, e.into()),
            },
            Command::Operations { run, limit } => match self.shared.queue.operations(&run, usize::try_from(limit).unwrap_or(usize::MAX)) {
                Ok(operations) => Reply::Operations { id, operations },
                Err(refusal) => refused(id, refusal),
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
            Command::ShutdownIfIdle => self.shutdown_if_idle(id),
            command => match self.shared.queue.submit(command) {
                Ok((operation, run, position)) => Reply::Accepted { id, operation, run, position },
                Err(refusal) => refused(id, refusal),
            },
        };
        self.outbox.send(reply).await.is_ok()
    }

    /// The connection's next request; none when it is to end: the client closed it or wrote a
    /// line that isn't a request (refused), the service stopped, or the tunnel of the connection's
    /// console ended (the client told why).
    async fn next_request(&mut self, reader: &mut LineReader<tokio::io::ReadHalf<Stream>>, closed: &mut tokio::sync::watch::Receiver<bool>) -> Option<Request> {
        let next = tokio::select! {
            next = reader.next::<Request>() => Next::Read(next),
            _ = closed.wait_for(|stopped| *stopped) => Next::Stopped,
            (id, reason) = self.tunnel_closed() => Next::TunnelEnded(id, reason),
        };
        match next {
            Next::Read(Ok(request)) => request,
            Next::Read(Err(e)) => {
                let refused = error(0, ErrorCode::Usage, format!("a line that isn't a request: {e}"));
                drop(self.outbox.send(refused).await);
                None
            }
            Next::Stopped => None,
            Next::TunnelEnded(id, reason) => {
                self.console = None;
                drop(self.outbox.send(Reply::ConsoleClosed { id, reason }).await);
                None
            }
        }
    }

    /// Waits for the tunnel of this connection's graphical console to end on its own: its
    /// request's id, and how it ended. Never, while it has none.
    async fn tunnel_closed(&mut self) -> (u64, String) {
        match &mut self.console {
            Some((id, tunnel)) => (*id, tunnel.closed().await),
            None => std::future::pending().await,
        }
    }

    /// Answers `command`, a question about the home that runs no operation: its reply, or none
    /// when the engine answers it on a task of its own (it reads the tools, and doesn't hold the
    /// connection's other requests).
    async fn ask(&mut self, id: u64, command: Command) -> Option<Reply> {
        Some(match command {
            Command::GraphicalConsole { .. } => self.graphical_console(id, command).await,
            Command::Record { run } => match self.shared.store.read(|tx| tx.run(&run)) {
                Ok(row) => Reply::Run { id, run: row.map(Box::new) },
                Err(e) => refused(id, e.into()),
            },
            Command::Forget { run } => self.forget(id, &run),
            Command::Expired => match runs(&self.shared.store) {
                Ok(runs) => {
                    let now = std::time::SystemTime::now();
                    let runs = runs.into_iter().filter(|r| r.record.expired(now)).collect();
                    Reply::Runs { id, runs }
                }
                Err(e) => refused(id, e.into()),
            },
            command => {
                let (shared, outbox) = (Arc::clone(&self.shared), self.outbox.clone());
                tokio::spawn(async move {
                    let reply = engine_answer(&shared, id, command).await.unwrap_or_else(|refusal| refused(id, refusal));
                    drop(outbox.send(reply).await);
                });
                return None;
            }
        })
    }

    /// The graphical console `command` asks for, its tunnel kept for as long as this connection
    /// (a second console's replaces it).
    async fn graphical_console(&mut self, id: u64, command: Command) -> Reply {
        let Command::GraphicalConsole { run, target, options, machine } = command else {
            return error(id, ErrorCode::Usage, "not a graphical console".to_owned());
        };
        let at = Opened {
            run: &run,
            target: target.as_ref(),
        };
        match self.shared.engine.graphical_console(at, &options, &machine).await {
            Ok(mut console) => match ReachedConsole::of(&console) {
                Some(reached) => {
                    self.console = console.tunnel.take().map(|tunnel| (id, tunnel));
                    Reply::Console {
                        id,
                        console: Box::new(reached),
                    }
                }
                None => error(
                    id,
                    ErrorCode::Unsupported,
                    format!("`{machine}`'s console answers where this protocol can't say"),
                ),
            },
            Err(refusal) => refused(id, refusal),
        }
    }

    /// Forgets run `run`'s record, unless an operation of it is queued or running.
    fn forget(&self, id: u64, run: &RunName) -> Reply {
        if self.shared.queue.under_way(run) {
            return error(
                id,
                ErrorCode::Locked,
                format!("an operation of `{run}` is queued or running: it is forgotten once none is"),
            );
        }
        match self.shared.store.write(|tx| tx.forget_run(run)) {
            Ok(()) => Reply::Ok { id },
            Err(e) => refused(id, e.into()),
        }
    }

    /// The refusal of `request` on a connection greeted in another protocol: it takes only
    /// `shutdown_if_idle`.
    fn refused_as_other(&self, request: &Request) -> Option<Reply> {
        (self.greeted == Greeted::Other && request.command != Command::ShutdownIfIdle).then(|| {
            error(
                request.id,
                ErrorCode::Usage,
                format!("this service speaks protocol {PROTOCOL}: a client of another takes only `shutdown_if_idle`"),
            )
        })
    }

    /// Stops the service when no operation is queued or running; else the ones that are.
    fn shutdown_if_idle(&self, id: u64) -> Reply {
        match self.shared.queue.stop_if_idle() {
            Ok(()) => {
                self.shared.request_stop();
                Reply::Ok { id }
            }
            Err(operations) => Reply::Busy { id, operations },
        }
    }

    /// Answers the connection's first request, which must be a `hello`: of any protocol, as every
    /// protocol keeps it, answered with this service's protocol and version.
    async fn greet(&mut self, request: Request) -> bool {
        let reply = match request.command {
            Command::Hello { protocol } => {
                self.greeted = if protocol == PROTOCOL { Greeted::Same } else { Greeted::Other };
                hello(request.id)
            }
            _ => error(request.id, ErrorCode::Usage, "the first request must be `hello`".to_owned()),
        };
        self.outbox.send(reply).await.is_ok() && self.greeted != Greeted::Not
    }

    /// Starts the connection's subscription (replacing one it had), registered before its `ok`
    /// is sent, and the `ok` sent before its first event: no event after it is missed. False
    /// when the client left.
    async fn subscribe(&mut self, id: u64, run: Option<RunName>, after: Option<EventCursor>) -> bool {
        if let Some(old) = self.subscription.take() {
            old.abort();
        }
        if self.outbox.send(Reply::Ok { id }).await.is_err() {
            return false;
        }
        let (feed, outbox) = (Arc::clone(&self.shared.feed), self.outbox.clone());
        self.subscription = Some(tokio::spawn(forward(feed, run, after, outbox)));
        true
    }
}

/// The reply to a `hello`: this service's protocol and version.
fn hello(id: u64) -> Reply {
    Reply::Hello {
        id,
        protocol: PROTOCOL,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// An error reply.
fn error(id: u64, code: ErrorCode, message: String) -> Reply {
    Reply::Error {
        id,
        report: isothrum_runner::ErrorReport::new(code, message).unchanged(),
    }
}

/// The engine's answer to `command`, request `id`: a status, the way into a machine or its
/// hypervisor's app, its endpoints, the runner's key, the downloads and their removal.
async fn engine_answer(shared: &Shared, id: u64, command: Command) -> Result<Reply, Refusal> {
    let engine = &shared.engine;
    Ok(match command {
        Command::Status { run, target, options } => {
            // One read of a run at a time; one asked meanwhile takes the next one's answer.
            let asked = crate::reads::Asked { target, options };
            let read = || engine.status(&run, asked.target.as_ref(), &asked.options);
            let status = shared.reads.status(&run, asked.clone(), read).await?;
            Reply::Status { id, status: Box::new(status) }
        }
        Command::ConsoleCommand { run, target, machine, user } => {
            let at = Opened {
                run: &run,
                target: target.as_ref(),
            };
            let shell = engine.console_command(at, &machine, user.as_deref()).await?;
            Reply::Shell { id, command: shell }
        }
        Command::NativeConsole { run, target, options, machine } => {
            let at = Opened {
                run: &run,
                target: target.as_ref(),
            };
            let console = engine.native_console(at, &options, &machine).await?;
            Reply::Native { id, console }
        }
        Command::ServiceEndpoints { run, target, options } => {
            let at = Opened {
                run: &run,
                target: target.as_ref(),
            };
            let endpoints = engine.service_endpoints(at, &options).await?;
            Reply::Endpoints { id, endpoints }
        }
        Command::PublicKey => Reply::PublicKey {
            id,
            key: engine.public_key().await?,
        },
        Command::Downloads => Reply::Downloads {
            id,
            downloads: engine.downloads().await?,
        },
        Command::RemoveDownloads { which } => Reply::DownloadsRemoved {
            id,
            removed: engine.remove_downloads(&which).await?,
        },
        Command::AdoptDownloads { adopted } => Reply::DownloadsAdopted {
            id,
            adopted: engine.adopt_downloads(&adopted).await?,
        },
        _ => return Err(Refusal::new(ErrorCode::Usage, "not a question the engine answers")),
    })
}

/// The error reply of `refusal`, with the runner's typed fields when it has them.
fn refused(id: u64, refusal: crate::engine::Refusal) -> Reply {
    Reply::Error { id, report: *refusal.0 }
}

/// Sends a subscription's events to `outbox`: those the store keeps after `after` (every one
/// when `None`), then each new one as it is kept, those of `run` only when it names one.
async fn forward(feed: Arc<Feed>, run: Option<RunName>, after: Option<EventCursor>, outbox: mpsc::Sender<Reply>) {
    let mut news = feed.news();
    let mut closed = feed.closed();
    let mut last = after.unwrap_or_default();
    loop {
        // Marked seen before reading: an event kept after the read wakes the next wait.
        news.mark_unchanged();
        let (batch, scanned) = match feed.after(last, run.as_ref()) {
            Ok(page) => page,
            Err(refusal) => {
                tracing::warn!(error = %refusal.0.message, "a subscription couldn't read the store");
                return;
            }
        };
        for (cursor, event) in batch {
            let frame = Reply::Event {
                cursor,
                event: Box::new(event),
            };
            if outbox.send(frame).await.is_err() {
                return;
            }
        }
        if scanned == last {
            // Everything kept is sent: once the service stopped, that is the end.
            if *closed.borrow() {
                return;
            }
            // Nothing new: wait until something is kept, or the service stops (read once more).
            tokio::select! {
                changed = news.changed() => if changed.is_err() { return },
                _ = closed.changed() => {}
            }
        }
        last = scanned;
    }
}

/// Every run the store keeps.
fn runs(store: &isothrum_runner::store::Store) -> isothrum_runner::Result<Vec<isothrum_client::v1::RunRow>> {
    store.read(every_run)
}

/// Every run `tx` reads: a function of any transaction (the method alone is bound to one
/// transaction's lifetime).
fn every_run(tx: &isothrum_runner::store::Tx<'_>) -> isothrum_runner::Result<Vec<isothrum_client::v1::RunRow>> {
    tx.runs()
}
