//! The Isothrum runner service: one daemon per runner home (`isothrum serve --home <home>`).
//!
//! It holds the home ([`Service::hold`]: an OS lock, one service per home), listens on an
//! owner-only local endpoint (a Unix domain socket in the home, a Windows named pipe only the
//! user may open) and speaks `isothrum-client`'s protocol: each run's operations are queued and
//! run one after the other, different runs' side by side, and every operation's events go to every
//! subscriber, numbered so a reconnecting one resumes where it left off. It stops on a `shutdown`
//! request, on its stop signal, or once idle (no client, no operation) for a while: queued
//! operations are dropped, running ones cancelled, and the tools they started stopped.
//!
//! The service knows nothing of the tools a run drives: operations go to an [`Engine`]
//! ([`RunnerEngine`]: the Isothrum runner).

mod endpoint;
mod engine;
mod error;
mod feed;
mod lease;
mod queue;
mod reads;
mod reaper;
mod session;

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use interprocess::local_socket::traits::tokio::Listener as _;
use isothrum_client::proto::PROTOCOL;
use isothrum_client::{BUILD_ENV, ServiceInfo, ServicePaths, build_of};
use isothrum_runner::SystemProcesses;
use isothrum_runner::store::{LeaseOutcome, Owner, Store};
use tokio::sync::Notify;

pub use engine::{Engine, Journal, Opened, Refusal, RunnerEngine};
pub use error::ServeError;

use crate::feed::Feed;
use crate::lease::Lease;
use crate::queue::Queue;

/// How long the running operations have to end once cancelled, before their tools are ended at
/// once: the runner asks each tool to stop and ends it 30 seconds later.
const DRAIN: Duration = Duration::from_secs(40);

/// How long the connections have to write what they hold once the service stopped.
const SESSIONS_END: Duration = Duration::from_secs(5);

/// How long the tools have to be gone once ended at once.
const TOOLS_GONE: Duration = Duration::from_secs(5);

/// A future that resolves when the service is asked to stop (a signal); called again after the
/// first, it resolves on the next ask, which ends the tools at once.
pub type StopSignal = Box<dyn FnMut() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

/// A home held by this process, before it serves.
#[derive(Debug)]
pub struct Service {
    paths: ServicePaths,
    lease: Lease,
}

/// How the service runs.
#[non_exhaustive]
pub struct ServeOptions {
    /// What runs the operations.
    pub engine: Arc<dyn Engine>,
    /// The tools' processes, ended at once if the operations don't end in time when the service
    /// stops.
    pub tools: Option<SystemProcesses>,
    /// Stops once this long with no client and no operation (never when `None`).
    pub idle: Option<Duration>,
    /// Asks the service to stop.
    pub stop: StopSignal,
    /// How often the reaper looks for runs past their lifetime and queues their `down` (never
    /// when `None`).
    pub reap_every: Option<Duration>,
}

impl ServeOptions {
    /// Operations on `engine`, stopped by `stop`, never idle out, no tools to end.
    #[must_use]
    pub fn new(engine: Arc<dyn Engine>, stop: StopSignal) -> ServeOptions {
        ServeOptions {
            engine,
            tools: None,
            idle: None,
            stop,
            reap_every: Some(reaper::EVERY),
        }
    }
}

/// What the connections share.
pub(crate) struct Shared {
    engine: Arc<dyn Engine>,
    queue: Arc<Queue>,
    feed: Arc<Feed>,
    store: Arc<Store>,
    reads: reads::Reads,
    clients: AtomicUsize,
    stop: Notify,
}

impl Shared {
    /// Counts a client until the guard drops.
    fn client_connected(self: &Arc<Self>) -> ClientGuard {
        self.clients.fetch_add(1, Ordering::SeqCst);
        ClientGuard(Arc::clone(self))
    }

    /// Asks the service to stop (a `shutdown` request).
    fn request_stop(&self) {
        self.stop.notify_one();
    }
}

/// A connected client, counted while it lives.
struct ClientGuard(Arc<Shared>);

impl Drop for ClientGuard {
    fn drop(&mut self) {
        self.0.clients.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Why the service stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    Asked,
    Signal,
    Idle,
}

impl Service {
    /// Holds `home` (creating it when missing): one service per home.
    ///
    /// # Errors
    ///
    /// [`ServeError::Held`] when another service holds it; `io` when the home or its lock file
    /// can't be created.
    pub fn hold(home: impl Into<PathBuf>) -> Result<Service, ServeError> {
        let home = home.into();
        std::fs::create_dir_all(&home).map_err(|source| ServeError::Io {
            what: "the home",
            path: home.clone(),
            source,
        })?;
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        let paths = ServicePaths::new(home);
        let lease = Lease::acquire(&paths)?;
        Ok(Service { paths, lease })
    }

    /// The home held.
    #[must_use]
    pub fn home(&self) -> &std::path::Path {
        self.paths.home()
    }

    /// Serves until asked to stop, idle, or stopped by `options.stop`; then drops the queued
    /// operations, cancels the running ones and stops their tools.
    ///
    /// # Errors
    ///
    /// The endpoint can't be opened, or `service.json` written.
    pub async fn serve(mut self, options: ServeOptions) -> Result<(), ServeError> {
        let ServeOptions {
            engine,
            tools,
            idle,
            mut stop,
            reap_every,
        } = options;
        let store = Arc::new(Store::open(self.paths.home())?);
        let owner = Owner::this_process();
        let _lease = HomeLease::take(&self.paths, &store, &owner)?;
        cancel_orphaned(&store);
        let (listener, endpoint) = endpoint::listen(&self.paths)?;
        let feed = Arc::new(Feed::new(Arc::clone(&store)));
        let queue = Arc::new(Queue::new(Arc::clone(&engine), Arc::clone(&store), Arc::clone(&feed), owner));
        let shared = Arc::new(Shared {
            engine,
            queue: Arc::clone(&queue),
            feed,
            store,
            reads: reads::Reads::default(),
            clients: AtomicUsize::new(0),
            stop: Notify::new(),
        });
        let info = ServiceInfo {
            pid: std::process::id(),
            started: isothrum_runner::rfc3339(SystemTime::now()),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: PROTOCOL,
            endpoint,
            epoch: random_u64(),
            build: build(),
        };
        self.lease.publish(&info)?;
        tracing::info!(home = %self.paths.home().display(), endpoint = %info.endpoint, pid = info.pid, "the runner service listens");
        let reaping = reap_every.map(|every| tokio::spawn(reaper::reap(Arc::clone(&shared.engine), Arc::clone(&queue), every)));
        let stopped = stop();
        let mut sessions = tokio::task::JoinSet::new();
        let why = accept(&listener, &shared, &mut sessions, idle, stopped).await;
        if let Some(reaping) = reaping {
            reaping.abort();
        }
        tracing::info!(?why, "the runner service stops");
        drop(listener);
        stop_operations(&queue, tools.as_ref(), &mut stop).await;
        end_sessions(&shared, sessions).await;
        #[cfg(unix)]
        if let Err(e) = std::fs::remove_file(&info.endpoint) {
            tracing::debug!(error = %e, "the socket couldn't be removed");
        }
        Ok(())
    }
}

/// Ends the clients' connections once the service stopped: each sends what was answered and
/// what its subscription has left to send, then closes; one still open after [`SESSIONS_END`]
/// is cut. None outlives the service, which no longer holds the home.
async fn end_sessions(shared: &Shared, mut sessions: tokio::task::JoinSet<()>) {
    shared.feed.close();
    let ended = tokio::time::timeout(SESSIONS_END, async { while sessions.join_next().await.is_some() {} }).await;
    if ended.is_err() {
        tracing::debug!(open = sessions.len(), "connections still open as the service stops are cut");
    }
    sessions.abort_all();
}

/// Accepts clients, each served on a task of `sessions`, until a `shutdown`, `stopped`, or `idle`
/// with no client and no operation.
async fn accept(
    listener: &interprocess::local_socket::tokio::Listener,
    shared: &Arc<Shared>,
    sessions: &mut tokio::task::JoinSet<()>,
    idle: Option<Duration>,
    stopped: Pin<Box<dyn Future<Output = ()> + Send>>,
) -> Why {
    tokio::pin!(stopped);
    let mut idle_since: Option<Instant> = None;
    let period = idle.map_or(Duration::from_secs(60), |d| (d / 4).clamp(Duration::from_millis(50), Duration::from_secs(1)));
    let mut tick = tokio::time::interval(period);
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(stream) => {
                    idle_since = None;
                    sessions.spawn(session::serve(stream, Arc::clone(shared)));
                    // The ones that ended, reaped: the set holds the open connections only.
                    while sessions.try_join_next().is_some() {}
                }
                Err(e) => tracing::warn!(error = %e, "a connection couldn't be accepted"),
            },
            () = shared.stop.notified() => return Why::Asked,
            () = &mut stopped => return Why::Signal,
            _ = tick.tick() => {
                let Some(limit) = idle else { continue };
                if shared.clients.load(Ordering::SeqCst) > 0 || shared.queue.busy() {
                    idle_since = None;
                } else if idle_since.get_or_insert_with(Instant::now).elapsed() >= limit {
                    return Why::Idle;
                }
            }
        }
    }
}

/// Drops the queued operations and cancels the running ones; once they ended (or after
/// [`DRAIN`], or on the next stop signal), ends every tool still running at once.
async fn stop_operations(queue: &Queue, tools: Option<&SystemProcesses>, stop: &mut StopSignal) {
    queue.close_all();
    let again = stop();
    tokio::select! {
        () = queue.drained() => {}
        () = tokio::time::sleep(DRAIN) => tracing::warn!("the operations didn't end in time: their tools are ended now"),
        () = again => tracing::warn!("asked again: the tools are ended now"),
    }
    if let Some(tools) = tools {
        tools.end_all_now();
        if tokio::time::timeout(TOOLS_GONE, tools.stopped()).await.is_err() {
            tracing::warn!("some tools were still running when the service stopped");
        }
    }
    // The subscribers get the operations' last events before the connections close.
    tokio::time::sleep(Duration::from_millis(100)).await;
}

/// Ends cancelled the operations a dead service queued and never started: nothing will run them,
/// and the journal shouldn't show them waiting.
fn cancel_orphaned(store: &Store) {
    match store.write(orphans_cancelled) {
        Ok(ids) if !ids.is_empty() => tracing::info!(count = ids.len(), "a dead service's queued operations were cancelled"),
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, "a dead service's queued operations couldn't be cancelled"),
    }
}

/// The orphaned queued operations `tx` cancels: a function of any transaction (the method alone
/// is bound to one transaction's lifetime).
fn orphans_cancelled(tx: &mut isothrum_runner::store::Tx<'_>) -> isothrum_runner::Result<Vec<isothrum_runner::store::OperationId>> {
    tx.cancel_orphaned_queued()
}

/// The build this service runs: the one the client that started it gave ([`BUILD_ENV`]: of the
/// binary it started, which may be a wrapper of this one), else this executable's.
fn build() -> Option<String> {
    std::env::var(BUILD_ENV)
        .ok()
        .filter(|build| !build.is_empty())
        .or_else(|| std::env::current_exe().ok().and_then(|exe| build_of(&exe)))
}

/// A random number (the time's, if the OS has no randomness to give).
fn random_u64() -> u64 {
    getrandom::u64().unwrap_or_else(|_| {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos());
        u64::try_from(nanos % u128::from(u64::MAX)).unwrap_or(1)
    })
}

/// The store's `home:service` lease, held while the service runs (renewed, released when
/// dropped): with the OS lock on `service.lock`, what says the home has a service.
struct HomeLease {
    store: Arc<Store>,
    owner: Owner,
    renew: tokio::task::JoinHandle<()>,
}

/// The lease's scope.
const SERVICE_LEASE: &str = "home:service";

/// How long the lease lasts unless renewed, and how often it is.
const LEASE_TTL: Duration = Duration::from_secs(60);
const LEASE_RENEW: Duration = Duration::from_secs(20);

impl HomeLease {
    /// Takes the lease for `owner`.
    ///
    /// # Errors
    ///
    /// [`ServeError::Held`] when a live process holds it (a service of another version that
    /// holds no OS lock); the store's.
    fn take(paths: &ServicePaths, store: &Arc<Store>, owner: &Owner) -> Result<HomeLease, ServeError> {
        let outcome = store.write(|tx| tx.acquire_lease(SERVICE_LEASE, owner, LEASE_TTL))?;
        let LeaseOutcome::Acquired { took_over } = &outcome else {
            if let LeaseOutcome::Held(lease) = &outcome {
                tracing::warn!(pid = lease.owner.pid, "another live process holds the home's service lease");
            }
            return Err(ServeError::Held {
                home: paths.home().to_path_buf(),
                holder: paths.read_info().map(Box::new),
            });
        };
        if let Some(left) = took_over {
            tracing::info!(pid = left.owner.pid, "took over a dead service's lease");
        }
        let (renewing, renewer) = (Arc::clone(store), owner.clone());
        let renew = tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEASE_RENEW).await;
                match renewing.write(|tx| tx.renew_lease(SERVICE_LEASE, &renewer, LEASE_TTL)) {
                    Ok(true) => {}
                    Ok(false) => tracing::warn!("the service's lease was lost"),
                    Err(e) => tracing::warn!(error = %e, "the service's lease couldn't be renewed"),
                }
            }
        });
        Ok(HomeLease {
            store: Arc::clone(store),
            owner: owner.clone(),
            renew,
        })
    }
}

impl Drop for HomeLease {
    fn drop(&mut self) {
        self.renew.abort();
        if let Err(e) = self.store.write(|tx| tx.release_lease(SERVICE_LEASE, &self.owner)) {
            tracing::warn!(error = %e, "the service's lease couldn't be released");
        }
    }
}

/// 128 random bits in hex.
#[cfg(windows)]
pub(crate) fn random_hex() -> String {
    format!("{:016x}{:016x}", random_u64(), random_u64())
}
