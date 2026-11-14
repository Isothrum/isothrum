//! Starting a home's service when none answers: `isothrum serve --home <home> --detach`, in a
//! process group (Windows: a process group with no console, out of the starter's job) of its own,
//! so the starter's Ctrl-C and its end don't reach it, its output appended to `service.log`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use crate::discovery::ServicePaths;

/// A log past this size is moved to `service.log.1` before a service starts.
const LOG_ROLL_BYTES: u64 = 8 * 1024 * 1024;

/// The `isothrum` binary to start: `binary` when given, else the one beside this program, else
/// the one on `PATH`.
pub(crate) fn binary(binary: Option<&Path>) -> PathBuf {
    if let Some(binary) = binary {
        return binary.to_path_buf();
    }
    let name = format!("isothrum{}", std::env::consts::EXE_SUFFIX);
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(&name)))
        .filter(|beside| beside.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Starts `binary`'s service for the home of `paths`, detached.
///
/// # Errors
///
/// The home or its log can't be created, or the binary can't be started.
#[expect(
    clippy::disallowed_methods,
    reason = "the client starts the service, a detached daemon that outlives it, not a tool call"
)]
pub(crate) fn start(binary: &Path, paths: &ServicePaths) -> std::io::Result<Child> {
    create_home(paths.home())?;
    let log_path = paths.log();
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > LOG_ROLL_BYTES)
        && let Err(e) = std::fs::rename(&log_path, log_path.with_extension("log.1"))
    {
        tracing::debug!(error = %e, "the service's log couldn't be rolled over");
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(&log_path)?;
    let mut command = Command::new(binary);
    command
        .arg("serve")
        .arg("--home")
        .arg(paths.home())
        .arg("--detach")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    detach(&mut command)
}

/// Creates `home` when missing (owner-only on Unix; the service makes it owner-only on Windows).
pub(crate) fn create_home(home: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(home)
}

/// Spawns `command` in a process group of its own.
#[cfg(unix)]
fn detach(command: &mut Command) -> std::io::Result<Child> {
    std::os::unix::process::CommandExt::process_group(command, 0);
    command.spawn()
}

/// Spawns `command` in a process group of its own, with no console, out of the starter's job
/// when the job lets it (else in it).
#[cfg(windows)]
fn detach(command: &mut Command) -> std::io::Result<Child> {
    use std::os::windows::process::CommandExt as _;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB);
    command.spawn().or_else(|_| {
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        command.spawn()
    })
}

/// The last lines of the service's log, for an error that says why it didn't start.
pub(crate) fn log_tail(paths: &ServicePaths) -> String {
    let text = std::fs::read_to_string(paths.log()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines.get(lines.len().saturating_sub(5)..).unwrap_or_default().join("\n")
}
