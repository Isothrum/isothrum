//! Making room for a service of this release ([`Client::connect_exact`](crate::Client::connect_exact)):
//! the home's service of another release is asked to stop when idle, or ended when it doesn't
//! answer and is surely the process `service.json` names, and waited for until it is gone.

use std::fs::{File, TryLockError};
use std::time::{Duration, Instant};

use isothrum_runner::store::{OperationId, Store};

use crate::VERSION;
use crate::discovery::{ServiceInfo, ServicePaths};
use crate::error::ClientError;
use crate::proto::{Command, PROTOCOL, Reply, Request};
use crate::spawn;
use crate::wire::{Reader, Writer, connect, write_line};

/// How long a service has to answer a `hello`, then a `shutdown_if_idle`.
const ANSWER_WAIT: Duration = Duration::from_secs(3);

/// How long a process that holds the home without answering may still be a service starting up.
const SILENT_WAIT: Duration = Duration::from_secs(5);

/// How long a service that agreed to stop has to be gone (an idle one ends at once).
const STOP_WAIT: Duration = Duration::from_secs(20);

/// How long an ended process has to be gone, after each signal.
const END_WAIT: Duration = Duration::from_secs(5);

/// How long a client waits for another one's replacement of the home's service.
const TURN_WAIT: Duration = Duration::from_secs(60);

/// How often the home is looked at while waiting.
const POLL: Duration = Duration::from_millis(50);

/// How many times the home is looked at again after a service was stopped, or was busy between
/// two operations, before giving up.
const ROUNDS: usize = 8;

/// How much later than its `started` the OS may say the service's process started (both read the
/// same clock; seconds are rounded, and the clock may have been nudged meanwhile).
const START_SLACK_S: u64 = 2;

type Result<T, E = ClientError> = std::result::Result<T, E>;

/// A connection past its `hello`, and what the service says of itself.
pub(crate) type Opened = (Reader, Writer, ServiceInfo);

/// The home's replacement turn, held while it lives.
pub(crate) struct Turn {
    _file: File,
}

impl Turn {
    /// Waits for the home's replacement turn (up to a minute).
    ///
    /// # Errors
    ///
    /// The home or its lock file can't be made, or another client kept the turn.
    pub(crate) async fn take(paths: &ServicePaths) -> Result<Turn> {
        spawn::create_home(paths.home())?;
        let file = File::options().create(true).truncate(false).write(true).open(paths.replace_lock())?;
        let deadline = Instant::now() + TURN_WAIT;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Turn { _file: file }),
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => tokio::time::sleep(POLL).await,
                Err(TryLockError::WouldBlock) => {
                    return Err(ClientError::Unavailable(format!(
                        "another client kept replacing the runner service of {} for a minute",
                        paths.home().display()
                    )));
                }
                Err(TryLockError::Error(e)) => return Err(e.into()),
            }
        }
    }
}

/// What the home has.
enum Found {
    /// No process holds it.
    Nothing,
    /// A service answered its `hello`: the connection, and its protocol.
    Answered(Opened, u32),
    /// A process holds it, and no service answered: what `service.json` says, when it says.
    Silent(Option<ServiceInfo>),
}

/// The connection to the home's service when it is this release's and protocol; else none, once
/// no process holds the home (the one of another release stopped or ended).
///
/// # Errors
///
/// [`ClientError::Busy`] when the service of another release has operations queued or running;
/// [`ClientError::Unavailable`] when it can't be stopped; [`ClientError::Protocol`] when it
/// refused to.
pub(crate) async fn make_room(paths: &ServicePaths) -> Result<Option<Opened>> {
    let (mut rounds, mut silent_since) = (0, None::<Instant>);
    while rounds < ROUNDS {
        match look(paths).await {
            Found::Nothing => return Ok(None),
            Found::Answered(opened, protocol) if protocol == PROTOCOL && opened.2.version == VERSION => return Ok(Some(opened)),
            Found::Answered(opened, _) => stop(paths, opened).await?,
            Found::Silent(info) => {
                // A service starting up holds the home before it says where it listens.
                if silent_since.get_or_insert_with(Instant::now).elapsed() < SILENT_WAIT {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                end_silent(paths, info).await?;
            }
        }
        silent_since = None;
        rounds += 1;
    }
    Err(ClientError::Unavailable(format!(
        "the runner service of {} couldn't be replaced: one kept coming back",
        paths.home().display()
    )))
}

/// What the home has now.
async fn look(paths: &ServicePaths) -> Found {
    let info = paths.read_info();
    if let Some(info) = &info
        && let Ok(Some((opened, protocol))) = tokio::time::timeout(ANSWER_WAIT, greet(info.clone())).await
    {
        return Found::Answered(opened, protocol);
    }
    if held(paths) { Found::Silent(info) } else { Found::Nothing }
}

/// A connection to the service `info` names past its `hello` (of this protocol: answered in any),
/// and the protocol it speaks; none when it doesn't answer one.
async fn greet(info: ServiceInfo) -> Option<(Opened, u32)> {
    let (mut reader, mut writer) = connect(&info.endpoint).await.ok()?;
    let hello = Request {
        id: 0,
        command: Command::Hello { protocol: PROTOCOL },
    };
    write_line(&mut writer, &hello).await.ok()?;
    match reader.next::<Reply>().await.ok()?? {
        Reply::Hello { protocol, version, .. } => Some(((reader, writer, ServiceInfo { version, ..info }), protocol)),
        _ => None,
    }
}

/// Whether a process holds the home's service lock (a lock file that can't be opened says yes).
fn held(paths: &ServicePaths) -> bool {
    let file = match File::options().write(true).open(paths.lock()) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    // Taken, the lock is let go at once, as the file closes.
    !matches!(file.try_lock(), Ok(()))
}

/// What a service said to `shutdown_if_idle`.
enum Asked {
    /// It stops.
    Stopping,
    /// It has these operations, and goes on (none: it was between two).
    Busy(Vec<OperationId>),
    /// No answer.
    Silent,
}

/// Stops the service of another release `opened` connects to, when idle, and waits until it is
/// gone (ending it if it agreed, and lingers).
///
/// # Errors
///
/// [`ClientError::Busy`] when it has operations; as [`end`].
async fn stop(paths: &ServicePaths, opened: Opened) -> Result<()> {
    let info = opened.2.clone();
    match ask_to_stop(opened).await? {
        Asked::Stopping => {
            if !gone(paths, &info, STOP_WAIT).await {
                tracing::warn!(pid = info.pid, "the runner service agreed to stop and didn't: it is ended");
                end(paths, &info).await?;
            }
            Ok(())
        }
        Asked::Busy(operations) if operations.is_empty() => {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Ok(())
        }
        Asked::Busy(operations) => Err(ClientError::Busy {
            version: info.version,
            pid: info.pid,
            operations,
        }),
        Asked::Silent => end_silent(paths, Some(info)).await,
    }
}

/// Sends `shutdown_if_idle` over `opened`, and reads its answer.
///
/// # Errors
///
/// [`ClientError::Protocol`] when the service refused it.
async fn ask_to_stop((mut reader, mut writer, info): Opened) -> Result<Asked> {
    let request = Request {
        id: 1,
        command: Command::ShutdownIfIdle,
    };
    if write_line(&mut writer, &request).await.is_err() {
        return Ok(Asked::Silent);
    }
    let answer = tokio::time::timeout(ANSWER_WAIT, async {
        loop {
            match reader.next::<Reply>().await {
                Ok(Some(reply)) if reply.id() != Some(1) => {}
                other => return other,
            }
        }
    });
    match answer.await {
        Ok(Ok(Some(Reply::Ok { .. }) | None)) => Ok(Asked::Stopping),
        Ok(Ok(Some(Reply::Busy { operations, .. }))) => Ok(Asked::Busy(operations)),
        Ok(Ok(Some(Reply::Error { report, .. }))) => Err(ClientError::Protocol(format!(
            "the runner service (Isothrum {}) refused to stop: {}",
            info.version, report.message
        ))),
        Ok(Ok(Some(other))) => Err(ClientError::Protocol(format!("expected the answer to `shutdown_if_idle`, got {other:?}"))),
        Ok(Err(_)) | Err(_) => Ok(Asked::Silent),
    }
}

/// Ends the service that holds the home and doesn't answer, unless the journal shows operations
/// of it unfinished.
///
/// # Errors
///
/// [`ClientError::Busy`] when the journal does; [`ClientError::Unavailable`] when it can't be
/// read, `service.json` says nothing, or as [`end`].
async fn end_silent(paths: &ServicePaths, info: Option<ServiceInfo>) -> Result<()> {
    let Some(info) = info.or_else(|| paths.read_info()) else {
        return Err(ClientError::Unavailable(format!(
            "a process holds the runner service of {} and says nothing of itself: it was left running",
            paths.home().display()
        )));
    };
    let operations = unfinished_of(paths, info.pid)?;
    if !operations.is_empty() {
        return Err(ClientError::Busy {
            version: info.version,
            pid: info.pid,
            operations,
        });
    }
    tracing::warn!(pid = info.pid, version = %info.version, "the runner service doesn't answer: it is ended");
    end(paths, &info).await
}

/// The operations of process `pid` the home's journal shows unfinished, read as the store is
/// (none when the home has no store).
///
/// # Errors
///
/// [`ClientError::Unavailable`] when the journal can't be read (another release's schema).
fn unfinished_of(paths: &ServicePaths, pid: u32) -> Result<Vec<OperationId>> {
    let rows = Store::read_existing(paths.home(), in_flight).map_err(|e| {
        ClientError::Unavailable(format!(
            "the runner service (pid {pid}) doesn't answer, and the journal can't be read to tell whether it runs operations ({e}): it was left running"
        ))
    })?;
    Ok(rows
        .unwrap_or_default()
        .into_iter()
        .filter(|row| row.owner.pid == pid)
        .map(|row| row.id)
        .collect())
}

/// The home's operations `tx` shows unfinished: a function of any transaction (the method alone
/// is bound to one transaction's lifetime).
fn in_flight(tx: &isothrum_runner::store::Tx<'_>) -> isothrum_runner::Result<Vec<isothrum_runner::store::OperationRow>> {
    tx.in_flight()
}

/// Ends the process `info` names while the home's lock is held and it is surely that process,
/// and waits until it is gone.
///
/// # Errors
///
/// [`ClientError::Unavailable`] when the process isn't surely the service, or doesn't end.
async fn end(paths: &ServicePaths, info: &ServiceInfo) -> Result<()> {
    for force in [false, true] {
        if !held(paths) {
            return Ok(());
        }
        if !is_the_service(info).await {
            return Err(ClientError::Unavailable(format!(
                "the runner service of {} doesn't answer, and pid {} isn't the process its service.json names (it started after): nothing was ended",
                paths.home().display(),
                info.pid
            )));
        }
        terminate(info.pid, force);
        if gone(paths, info, END_WAIT).await {
            return Ok(());
        }
    }
    Err(ClientError::Unavailable(format!("the runner service (pid {}) didn't end", info.pid)))
}

/// Whether process `info.pid` is the service that wrote `info`: a process with that pid started
/// no later than the service said it did. A pid is reused only after its process ended, so a
/// process that has it since started later.
async fn is_the_service(info: &ServiceInfo) -> bool {
    let Some(said) = isothrum_runner::rfc3339_seconds(&info.started) else {
        return false;
    };
    isothrum_runner::process_started_at(info.pid)
        .await
        .is_some_and(|started| started <= said.saturating_add(START_SLACK_S))
}

/// Whether the service `info` names is gone within `wait`: the home's lock let go, and nothing
/// listening where it did.
async fn gone(paths: &ServicePaths, info: &ServiceInfo, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if !held(paths) && connect(&info.endpoint).await.is_err() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Asks process `pid` to end (SIGTERM), or ends it (`force`: SIGKILL).
#[cfg(unix)]
fn terminate(pid: u32, force: bool) {
    use nix::sys::signal::{Signal, kill};
    let Some(pid) = i32::try_from(pid).ok().filter(|p| *p > 1).map(nix::unistd::Pid::from_raw) else {
        return;
    };
    let signal = if force { Signal::SIGKILL } else { Signal::SIGTERM };
    if let Err(e) = kill(pid, signal) {
        tracing::debug!(%pid, error = %e, "the runner service couldn't be signalled (it may have ended)");
    }
}

/// Ends process `pid` (`taskkill /F`: Windows has no signal to ask it first, so `force` changes
/// nothing).
#[cfg(windows)]
#[expect(
    clippy::disallowed_methods,
    reason = "the client ends a service of another release it replaces, with Windows' own taskkill: the runner's process module isn't the client's"
)]
fn terminate(pid: u32, _force: bool) {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let mut command = std::process::Command::new("taskkill");
    command
        .args(["/PID", &pid.to_string(), "/F"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW);
    if let Err(e) = command.status() {
        tracing::debug!(pid, error = %e, "taskkill couldn't run to end the runner service");
    }
}
