//! The client: one connection for requests and their replies, and one more per subscription.

mod console;
mod events;
mod home;
pub use console::HeldConsole;
use events::OperationEvents;
pub use events::{Followed, Subscription};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use isothrum_runner::{ErrorCode, ErrorReport, EventKind, InputVariable, Park, RunName, Status};
use tokio::sync::{mpsc, oneshot};

use crate::discovery::{ServiceInfo, ServicePaths};
use crate::error::ClientError;
use crate::proto::{Command, OpOptions, PROTOCOL, Reply, Request, Target};
use crate::v1::{Event, EventCursor, OperationId, OperationPhase, OperationRow, RunRow, is_outcome, phase_of};
use crate::wire::{Reader, Writer, connect, write_line};
use crate::{VERSION, replace, spawn};

/// How long a client waits for a service it started to answer: it connects as soon as the
/// service does, so this only bounds a start that hangs. A binary's first run can take long on
/// a loaded machine (macOS checks a new executable before it runs it).
const START_WAIT: Duration = Duration::from_secs(30);

/// How many of a subscription's events wait for their reader before the connection is read no
/// further (the service then sends what was missed as it can).
const SUBSCRIPTION_BUFFER: usize = 1024;

/// How many of a run's newest operations [`Client::follow_again`] reads: the one it may follow,
/// and those still ahead of it.
const OPERATIONS_LOOKED_AT: u32 = 16;

/// What `Client::connect` returns.
type Result<T, E = ClientError> = std::result::Result<T, E>;

/// An operation the service queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    /// Its id.
    pub operation: OperationId,
    /// Its run.
    pub run: RunName,
    /// How many of the run's operations are ahead of it (0: it begins now).
    pub position: u32,
}

/// A connection to a home's runner service.
pub struct Client {
    shared: Arc<Shared>,
    reader: tokio::task::JoinHandle<()>,
    paths: ServicePaths,
    info: ServiceInfo,
}

/// What the requests share with the task reading their replies.
struct Shared {
    writer: tokio::sync::Mutex<Writer>,
    pending: Mutex<HashMap<u64, oneshot::Sender<Reply>>>,
    next: AtomicU64,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("home", &self.paths.home())
            .field("service", &self.info)
            .finish_non_exhaustive()
    }
}

/// `home`, absolute, its links resolved when it exists: the same home reads the same from any
/// client.
fn resolve(home: &Path) -> PathBuf {
    let absolute = std::path::absolute(home).unwrap_or_else(|_| home.to_path_buf());
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

/// A connection to the service at `endpoint`, its `hello` sent, and the service's first answer.
async fn hello(endpoint: &str) -> Result<(Reader, Writer, Option<Reply>)> {
    let (mut reader, mut writer) = connect(endpoint)
        .await
        .map_err(|e| ClientError::Unavailable(format!("the service at {endpoint} doesn't answer: {e}")))?;
    write_line(
        &mut writer,
        &Request {
            id: 0,
            command: Command::Hello { protocol: PROTOCOL },
        },
    )
    .await?;
    let first = reader.next::<Reply>().await?;
    Ok((reader, writer, first))
}

/// One connection to the service of `paths`, past its `hello`, and what the service says of
/// itself (its version as its `hello` gives it).
async fn open(paths: &ServicePaths) -> Result<(Reader, Writer, ServiceInfo)> {
    let info = paths
        .read_info()
        .ok_or_else(|| ClientError::Unavailable(format!("no service runs for {}", paths.home().display())))?;
    let (reader, writer, first) = hello(&info.endpoint).await?;
    match first {
        Some(Reply::Hello { protocol, version, .. }) if protocol == PROTOCOL => Ok((reader, writer, ServiceInfo { version, ..info })),
        Some(Reply::Hello { protocol, version, .. }) => Err(ClientError::Protocol(format!(
            "the service (Isothrum {version}) speaks protocol {protocol}, this client {PROTOCOL}: stop it to let this version start its own"
        ))),
        Some(Reply::Error { report, .. }) => Err(ClientError::Protocol(report.message)),
        Some(other) => Err(ClientError::Protocol(format!("expected the service's hello, got {other:?}"))),
        None => Err(ClientError::closed()),
    }
}

impl Client {
    /// Connects to `home`'s service, starting it (`isothrum serve --home <home> --detach`:
    /// `binary` when given, else the `isothrum` beside this program, else the one on `PATH`)
    /// when none answers, and waiting up to 30 seconds for it (less once it ended without answering).
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] when none answers and none could be started (with the end of
    /// its log); [`ClientError::Protocol`] when the service speaks another protocol.
    pub async fn connect(home: &Path, binary: Option<&Path>) -> Result<Client> {
        let paths = ServicePaths::new(resolve(home));
        match open(&paths).await {
            Err(ClientError::Unavailable(_)) => {}
            opened => return opened.map(|(r, w, info)| Client::over(paths, r, w, info)),
        }
        Client::start(paths, binary).await
    }

    /// Connects to `home`'s service of this release ([`VERSION`]), protocol and build (of
    /// `binary`: [`ServiceInfo::build`]): the one that runs when it is, else after replacing the one
    /// of another release or build (stopped, or ended when it doesn't answer), else none; a service it then starts as [`Client::connect`] does
    /// (`binary`: the `isothrum` of this release). How it replaces a service is in the crate's
    /// documentation.
    ///
    /// # Errors
    ///
    /// [`ClientError::Busy`] when the service of another release has operations queued or
    /// running: it is left running; [`ClientError::Version`] when the service started is another
    /// release (`binary` isn't this one's); [`ClientError::Unavailable`] when the service couldn't
    /// be stopped (it doesn't answer, and can't be told from another process, or doesn't end) or
    /// none could be started; [`ClientError::Protocol`] when it refused to stop.
    pub async fn connect_exact(home: &Path, binary: Option<&Path>) -> Result<Client> {
        let paths = ServicePaths::new(resolve(home));
        let binary = spawn::binary(binary);
        let _turn = replace::Turn::take(&paths).await?;
        if let Some((r, w, info)) = replace::make_room(&paths, crate::build_of(&binary).as_deref()).await? {
            return Ok(Client::over(paths, r, w, info));
        }
        let client = Client::start(paths, Some(&binary)).await?;
        if client.info.version == VERSION {
            return Ok(client);
        }
        // Not this release's: it is left as it was found, with no service.
        if let Err(e) = client.request(Command::ShutdownIfIdle).await {
            tracing::debug!(error = %e, "the service of another release just started couldn't be stopped");
        }
        Err(ClientError::Version {
            client: VERSION.to_owned(),
            service: client.info.version.clone(),
        })
    }

    /// Starts `binary`'s service for the home of `paths` (see [`Client::connect`]), and connects
    /// to it.
    async fn start(paths: ServicePaths, binary: Option<&Path>) -> Result<Client> {
        let binary = spawn::binary(binary);
        let mut child = spawn::start(&binary, &paths).map_err(|e| ClientError::Unavailable(format!("{} couldn't be started: {e}", binary.display())))?;
        // Until it answers, hangs past START_WAIT, or ends without answering (a binary that
        // refuses `serve`, a home it can't open): waiting START_WAIT out for that one cost every
        // connection 30 s for an answer that never comes. Its end is read before the look, so
        // the look after it ended still meets a service that answers (another client's, which
        // took the home first and made this one end).
        let deadline = Instant::now() + START_WAIT;
        let (opened, ended) = loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let ended = child.try_wait().ok().flatten();
            match open(&paths).await {
                Err(ClientError::Unavailable(why)) if ended.is_none() && Instant::now() < deadline => {
                    tracing::trace!(%why, "waiting for the service");
                }
                opened => break (opened, ended),
            }
        };
        // The service outlives this client; a thread reaps it should it end first.
        std::thread::spawn(move || child.wait());
        match opened {
            Ok((r, w, info)) => Ok(Client::over(paths, r, w, info)),
            Err(ClientError::Unavailable(why)) => Err(ClientError::Unavailable(format!(
                "{why}; the service started from {} {}: {}",
                binary.display(),
                ended.map_or_else(|| "didn't answer".to_owned(), |status| format!("ended without answering ({status})")),
                spawn::log_tail(&paths)
            ))),
            Err(e) => Err(e),
        }
    }

    /// Connects to `home`'s service when one runs, starting none.
    ///
    /// # Errors
    ///
    /// [`ClientError::Unavailable`] when none answers.
    pub async fn connect_running(home: &Path) -> Result<Client> {
        let paths = ServicePaths::new(resolve(home));
        let (r, w, info) = open(&paths).await?;
        Ok(Client::over(paths, r, w, info))
    }

    /// The client over a connection past its `hello`.
    fn over(paths: ServicePaths, mut reader: Reader, writer: Writer, info: ServiceInfo) -> Client {
        let shared = Arc::new(Shared {
            writer: tokio::sync::Mutex::new(writer),
            pending: Mutex::default(),
            next: AtomicU64::new(1),
        });
        let replies = Arc::clone(&shared);
        let reader = tokio::spawn(async move {
            while let Ok(Some(reply)) = reader.next::<Reply>().await {
                let Some(id) = reply.id() else { continue };
                let waiting = replies.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                if let Some(waiting) = waiting {
                    // A caller that gave up on its reply dropped its receiver: nothing to do.
                    drop(waiting.send(reply));
                }
            }
            // Every caller still waiting learns the connection ended.
            replies.pending.lock().unwrap_or_else(PoisonError::into_inner).clear();
        });
        Client { shared, reader, paths, info }
    }

    /// What the service said of itself when it started.
    #[must_use]
    pub fn service(&self) -> &ServiceInfo {
        &self.info
    }

    /// Sends `command`, and returns its reply (an error reply as [`ClientError::Refused`]).
    ///
    /// # Errors
    ///
    /// The service refused it, or the connection ended: before `command` was sent
    /// ([`ClientError::Io`]), or after, unanswered ([`ClientError::Unanswered`]).
    pub async fn request(&self, command: Command) -> Result<Reply> {
        let id = self.shared.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
        let written = write_line(&mut *self.shared.writer.lock().await, &Request { id, command }).await;
        if let Err(e) = written {
            self.shared.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            return Err(e.into());
        }
        // Sent: an end of the connection now leaves unknown whether the service acted on it.
        match rx.await.map_err(|_| ClientError::Unanswered)? {
            Reply::Error { report, .. } => Err(ClientError::Refused(Box::new(report))),
            reply => Ok(reply),
        }
    }

    /// Queues the operation `command` asks for.
    ///
    /// # Errors
    ///
    /// The service refused it (the run can't be opened, another operation of the run is queued
    /// or running: [`ErrorCode::Locked`], the service is
    /// stopping), or `command` isn't an operation ([`ClientError::Protocol`]).
    pub async fn submit(&self, command: Command) -> Result<Accepted> {
        if command.op().is_none() {
            return Err(ClientError::Protocol(format!("not an operation: {command:?}")));
        }
        match self.request(command).await? {
            Reply::Accepted { operation, run, position, .. } => Ok(Accepted { operation, run, position }),
            other => Err(unexpected(&other)),
        }
    }

    /// Queues `up`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn up(&self, target: Target, options: OpOptions) -> Result<Accepted> {
        self.submit(Command::Up {
            target,
            options,
            lifetime_s: None,
        })
        .await
    }

    /// Queues `down`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn down(&self, target: Target, options: OpOptions) -> Result<Accepted> {
        self.submit(Command::Down { target, options }).await
    }

    /// Queues `park`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn park(&self, target: Target, options: OpOptions, mode: Park) -> Result<Accepted> {
        self.submit(Command::Park { target, options, mode }).await
    }

    /// Queues `resume`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn resume(&self, target: Target, options: OpOptions) -> Result<Accepted> {
        self.submit(Command::Resume { target, options }).await
    }

    /// Queues `update`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn update(&self, target: Target, options: OpOptions) -> Result<Accepted> {
        self.submit(Command::Update { target, options }).await
    }

    /// Queues `retire` of `machine`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn retire(&self, target: Target, options: OpOptions, machine: String) -> Result<Accepted> {
        self.submit(Command::Retire { target, options, machine }).await
    }

    /// Queues `attach` of `machine`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn attach(&self, target: Target, options: OpOptions, machine: String) -> Result<Accepted> {
        self.submit(Command::Attach { target, options, machine }).await
    }

    /// Queues `exec` of `argv` in `machine`.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`].
    pub async fn exec(&self, target: Target, options: OpOptions, machine: String, argv: Vec<String>) -> Result<Accepted> {
        self.submit(Command::Exec {
            target,
            options,
            machine,
            argv,
        })
        .await
    }

    /// Cancels `operation`: queued, it never begins; running, it is cut short.
    ///
    /// # Errors
    ///
    /// The service refused (an unknown or ended operation), or the connection ended.
    pub async fn cancel(&self, operation: OperationId) -> Result<()> {
        self.request(Command::Cancel { operation }).await.map(drop)
    }

    /// Run `run`'s status (`target`: where to open it, for a run not recorded yet; else as
    /// recorded).
    ///
    /// # Errors
    ///
    /// The runner couldn't read it, or the connection ended.
    pub async fn status(&self, run: &RunName, target: Option<Target>) -> Result<Status> {
        self.status_with(run, target, OpOptions::default()).await
    }

    /// [`Client::status`], the read's tools given `options`' values and secrets (a Proxmox VE
    /// module's API token, for its VMs' states on the server).
    ///
    /// # Errors
    ///
    /// As [`Client::status`]; `server_auth_failed` when a server refuses the credentials.
    pub async fn status_with(&self, run: &RunName, target: Option<Target>, options: OpOptions) -> Result<Status> {
        let command = Command::Status {
            run: run.clone(),
            target,
            options,
        };
        match self.request(command).await? {
            Reply::Status { status, .. } => Ok(*status),
            other => Err(unexpected(&other)),
        }
    }

    /// The input variables the Terraform folder `target` names declares (name, type, default,
    /// sensitive), read from its files: a sensitive one is given as a secret, which no record
    /// keeps.
    ///
    /// # Errors
    ///
    /// The folder isn't a Terraform module (`unsupported`, `project_invalid`), or the connection
    /// ended.
    pub async fn variables(&self, target: Target) -> Result<Vec<InputVariable>> {
        match self.request(Command::Variables { target }).await? {
            Reply::Variables { variables, .. } => Ok(variables),
            other => Err(unexpected(&other)),
        }
    }

    /// Every recorded run.
    ///
    /// # Errors
    ///
    /// The runner couldn't read them, or the connection ended.
    pub async fn list(&self) -> Result<Vec<RunRow>> {
        match self.request(Command::List).await? {
            Reply::Runs { runs, .. } => Ok(runs),
            other => Err(unexpected(&other)),
        }
    }

    /// A connection of its own to the service, past its `hello`.
    pub(super) async fn dedicated(&self) -> Result<(Reader, Writer)> {
        let (reader, writer, first) = hello(&self.info.endpoint).await?;
        match first {
            Some(Reply::Hello { .. }) => Ok((reader, writer)),
            Some(Reply::Error { report, .. }) => Err(ClientError::Refused(Box::new(report))),
            Some(other) => Err(unexpected(&other)),
            None => Err(ClientError::closed()),
        }
    }

    /// Run `run`'s latest operations, newest first.
    ///
    /// # Errors
    ///
    /// The connection ended.
    pub async fn operations(&self, run: &RunName, limit: u32) -> Result<Vec<OperationRow>> {
        match self.request(Command::Operations { run: run.clone(), limit }).await? {
            Reply::Operations { operations, .. } => Ok(operations),
            other => Err(unexpected(&other)),
        }
    }

    /// Stops the service: its operations are cancelled and their tools stopped.
    ///
    /// # Errors
    ///
    /// The connection ended before the service answered.
    pub async fn shutdown(&self) -> Result<()> {
        self.request(Command::Shutdown).await.map(drop)
    }

    /// The events after `after` (every one the service keeps when `None`), then the live ones, of
    /// run `run` or of every run, on a connection of their own.
    ///
    /// # Errors
    ///
    /// The service refused, or doesn't answer.
    pub async fn subscribe(&self, run: Option<RunName>, after: Option<EventCursor>) -> Result<Subscription> {
        let (mut reader, mut writer) = self.dedicated().await?;
        write_line(
            &mut writer,
            &Request {
                id: 1,
                command: Command::Subscribe { run, after },
            },
        )
        .await?;
        match reader.next::<Reply>().await? {
            Some(Reply::Ok { id: 1 }) => {}
            Some(Reply::Error { report, .. }) => return Err(ClientError::Refused(Box::new(report))),
            Some(other) => return Err(unexpected(&other)),
            None => return Err(ClientError::closed()),
        }
        let (tx, rx) = mpsc::channel(SUBSCRIPTION_BUFFER);
        let task = tokio::spawn(async move {
            // The writing half stays open as long as the subscription: closing it ends it.
            let _writer = writer;
            loop {
                let item = match reader.next::<Reply>().await {
                    Ok(Some(Reply::Event { cursor, event })) => Ok((cursor, *event)),
                    Ok(Some(_)) => continue,
                    Ok(None) => Err(ClientError::closed()),
                    Err(e) => Err(ClientError::Io(e)),
                };
                let last = item.is_err();
                if tx.send(item).await.is_err() || last {
                    return;
                }
            }
        });
        Ok(Subscription { rx, task })
    }

    /// Queues the operation `command` asks for and follows it: its events, from its `queued` to
    /// its `ended`. The subscription is open before the operation is queued, so none is missed.
    ///
    /// # Errors
    ///
    /// As [`Client::submit`] and [`Client::subscribe`].
    pub async fn follow(&self, command: Command) -> Result<Followed> {
        let run = command.target().and_then(|(target, _)| target.name.clone());
        let events = self.subscribe(run, None).await?;
        let accepted = self.submit(command).await?;
        let events = OperationEvents::new(accepted.operation, events);
        Ok(Followed { accepted, events })
    }

    /// Follows `command` once more, its first sending unanswered ([`ClientError::Unanswered`]):
    /// the service may have queued it. When the run's newest operation is of `command`'s kind and
    /// under way in this service (not ended, queued by this service's process), that one is
    /// followed, from its `queued`: it is the first sending's, or another client's same operation,
    /// which `command` would only have queued behind. Else `command` is queued anew
    /// ([`Client::follow`]): a service that died with it took it along (one restarted since
    /// recovers it as an operation of its own, under way, which is then the one followed). One
    /// that already ended, and a target that doesn't name its run, are queued anew too: an
    /// operation run twice brings the run to the same place.
    ///
    /// # Errors
    ///
    /// As [`Client::follow`], and the run's operations couldn't be read.
    pub async fn follow_again(&self, command: Command) -> Result<Followed> {
        let (Some(op), Some(run)) = (command.op(), command.target().and_then(|(target, _)| target.name.clone())) else {
            return self.follow(command).await;
        };
        // Subscribed first, as `follow` is: no event of the operation found is missed.
        let events = self.subscribe(Some(run.clone()), None).await?;
        let rows = self.operations(&run, OPERATIONS_LOOKED_AT).await?;
        let under_way = |r: &OperationRow| r.ended.is_none() && r.owner.pid == self.info.pid;
        let Some(newest) = rows.first().filter(|r| r.op == op && under_way(r)) else {
            drop(events);
            return self.follow(command).await;
        };
        tracing::info!(operation = %newest.id, "the unanswered operation was queued: following it, not queuing it again");
        let ahead = rows.iter().skip(1).filter(|r| under_way(r)).count();
        let accepted = Accepted {
            operation: newest.id,
            run,
            position: u32::try_from(ahead).unwrap_or(u32::MAX),
        };
        Ok(Followed {
            events: OperationEvents::new(accepted.operation, events),
            accepted,
        })
    }

    /// Waits for operation `operation`'s last event: its `result`, or its `error` as
    /// [`ClientError::Operation`] (a cancel before it began, as one of code `cancelled`).
    ///
    /// # Errors
    ///
    /// The operation failed, or the connection ended.
    pub async fn wait(&self, operation: OperationId) -> Result<Event> {
        let events = self.subscribe(None, None).await?;
        OperationEvents::new(operation, events).outcome().await
    }
}

/// A reply of a kind the request doesn't get.
pub(super) fn unexpected(reply: &Reply) -> ClientError {
    ClientError::Protocol(format!("unexpected reply: {reply:?}"))
}
