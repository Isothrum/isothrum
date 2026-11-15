//! Starting a home's service when none answers: `isothrum serve --home <home> --detach`, in a
//! process group of its own (Windows: on a console of its own with no window, out of the
//! starter's job), so the starter's Ctrl-C and its end don't reach it, its output appended to
//! `service.log`.

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

/// No console window (`CREATE_NO_WINDOW`): the service gets a console of its own, which the
/// starter's Ctrl-C never reaches.
#[cfg(any(windows, test))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Out of the starter's job object, when the job lets it: the starter's end doesn't end it.
#[cfg(any(windows, test))]
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

/// The creation flags the service is started with on Windows, the first the job lets through.
/// Never `CREATE_NEW_PROCESS_GROUP` (0x200): it turns Ctrl-C off in the new group (an implied
/// `SetConsoleCtrlHandler(NULL, TRUE)`), which every tool the service starts inherits, so a
/// cancelled `terraform apply` ignored the Ctrl-C the runner stops it with and was ended 30 s
/// later, its state at risk (#132). Its own console already keeps the starter's Ctrl-C away.
#[cfg(any(windows, test))]
const DETACHED: [u32; 2] = [CREATE_NO_WINDOW | CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW];

/// Spawns `command` on a console of its own with no window, out of the starter's job when the
/// job lets it (else in it), Ctrl-C left on for the tools it starts ([`DETACHED`]).
#[cfg(windows)]
fn detach(command: &mut Command) -> std::io::Result<Child> {
    use std::os::windows::process::CommandExt as _;
    command.creation_flags(DETACHED[0]);
    command.spawn().or_else(|_| {
        command.creation_flags(DETACHED[1]);
        command.spawn()
    })
}

/// The last lines of the service's log, for an error that says why it didn't start.
pub(crate) fn log_tail(paths: &ServicePaths) -> String {
    let text = std::fs::read_to_string(paths.log()).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines.get(lines.len().saturating_sub(5)..).unwrap_or_default().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (#132): the service started in a new process group turned Ctrl-C off for every
    /// tool it ran, so a cancelled Terraform apply was killed instead of stopping cleanly.
    #[test]
    fn the_service_starts_without_a_new_process_group_on_a_console_of_its_own() {
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        for flags in DETACHED {
            assert_eq!(flags & CREATE_NEW_PROCESS_GROUP, 0, "{flags:#x}: Ctrl-C off for the tools");
            assert_eq!(flags & CREATE_NO_WINDOW, CREATE_NO_WINDOW, "{flags:#x}: a console of its own");
        }
        assert_eq!(DETACHED[0] & CREATE_BREAKAWAY_FROM_JOB, CREATE_BREAKAWAY_FROM_JOB, "out of the job first");
    }

    #[test]
    fn the_binary_given_is_the_one_started_else_isothrum_by_name() {
        assert_eq!(
            binary(Some(Path::new("/opt/isothrum/bin/isothrum"))),
            PathBuf::from("/opt/isothrum/bin/isothrum")
        );
        let found = binary(None);
        let name = format!("isothrum{}", std::env::consts::EXE_SUFFIX);
        assert_eq!(found.file_name().and_then(|n| n.to_str()), Some(name.as_str()));
    }

    #[test]
    fn the_logs_tail_is_its_last_five_lines_and_no_log_says_nothing() {
        let home = tempfile::tempdir().expect("a home");
        let paths = ServicePaths::new(home.path());
        assert_eq!(log_tail(&paths), "", "no log yet");
        let lines: Vec<String> = (1..=7).map(|n| format!("line {n}")).collect();
        std::fs::write(paths.log(), lines.join("\n")).expect("a log");
        assert_eq!(log_tail(&paths), "line 3\nline 4\nline 5\nline 6\nline 7");
    }
}
