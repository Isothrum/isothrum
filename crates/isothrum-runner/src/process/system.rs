//! Real processes, through `process-wrap` (no `unsafe` here): each tool in its own process group
//! (unix) or job object (Windows), so a timeout or a cancellation ends the whole tree it started
//! (`vagrant` is a wrapper around `ruby`, `cmd /C az` around `python`), headless on Windows. A
//! tool cut short is asked to stop first, on Unix (see [`super::stopping`]).

use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::process::Command;

use super::stopping::{GRACE, Stopping};
use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, ledger, lines, shim};
use crate::BoxFuture;

/// Starts real processes. [`SystemProcesses::default`] reads the Docker CLI's config
/// (`$DOCKER_CONFIG/config.json`, else `~/.docker/config.json`) to learn whether the user picked
/// an engine context of their own.
///
/// A tool cut short (its operation's future dropped: a cancel, a step's timeout) is asked to stop
/// on Unix (SIGINT to its process group, so Terraform saves its state) and ended once it has
/// exited, or 30 seconds later; on Windows it is ended at once. Its clones share the tools
/// stopping: a process about to exit waits for them with [`SystemProcesses::stopped`].
#[derive(Debug, Clone)]
pub struct SystemProcesses {
    /// Drop `DOCKER_HOST` and `DOCKER_CONTEXT` for `docker` (the user picked a context).
    strip_engine_overrides: bool,
    /// The tools cut short that are still stopping.
    stopping: Arc<Stopping>,
    /// The process groups (unix) or trees (Windows) of the tools running now, by leader pid.
    running: Arc<std::sync::Mutex<std::collections::BTreeSet<u32>>>,
}

impl Default for SystemProcesses {
    fn default() -> SystemProcesses {
        SystemProcesses::for_host(|v| std::env::var_os(v), std::env::home_dir().as_deref())
    }
}

/// The Docker CLI's config at `path`: none when there is none, and when it can't be read, which is
/// traced (whether the user picked a context is then unknown, so the inherited engine variables
/// stay).
fn read_cli_config(path: &Path) -> Option<String> {
    crate::store::if_there(std::fs::read_to_string(path)).unwrap_or_else(|e| {
        tracing::warn!(
            path = %path.display(),
            error = %e,
            "the Docker CLI's config couldn't be read: DOCKER_HOST and DOCKER_CONTEXT are kept for docker"
        );
        None
    })
}

impl SystemProcesses {
    /// Real processes, the Docker CLI's config read from `$DOCKER_CONFIG/config.json`, else
    /// `<home>/.docker/config.json` (`var` reads the environment, `home` is the user's): it says
    /// whether the user picked an engine context of their own. A `DOCKER_CONFIG` that isn't UTF-8
    /// is a path like any other.
    fn for_host(var: impl Fn(&str) -> Option<std::ffi::OsString>, home: Option<&Path>) -> SystemProcesses {
        let dir = var("DOCKER_CONFIG").map(std::path::PathBuf::from).or_else(|| home.map(|h| h.join(".docker")));
        let config = dir.and_then(|d| read_cli_config(&d.join("config.json")));
        SystemProcesses {
            strip_engine_overrides: shim::strips_engine_overrides(config.as_deref()),
            stopping: Arc::new(Stopping::new(GRACE)),
            running: Arc::default(),
        }
    }

    /// Whether a tool cut short is still stopping.
    #[must_use]
    pub fn is_stopping(&self) -> bool {
        self.stopping.any()
    }

    /// Once every tool cut short has ended: at the latest 30 seconds after the last was cut short.
    /// At once when none is stopping.
    pub async fn stopped(&self) {
        self.stopping.stopped().await;
    }

    /// Ends every tool cut short that is still stopping, at once (a second Ctrl-C).
    pub fn end_stopping(&self) {
        self.stopping.end_now();
    }

    /// Ends every tool running now and every one still stopping, at once: for an embedder about to
    /// exit, whose futures are never dropped (a status read's `docker compose ps` would outlive it).
    pub fn end_all_now(&self) {
        self.stopping.end_now();
        let running = std::mem::take(&mut *self.running.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
        for pid in running {
            kill_tree(pid);
        }
    }

    async fn run_to_end(&self, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(arg) = shim::refused_arg(&spec.program, &spec.args) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                why: format!("its argument `{}` holds a character cmd.exe would interpret", arg.escape_debug()),
            });
        }
        let io = |source| ProcessError::Io {
            program: spec.program.clone(),
            source,
        };
        // The program and its arguments only: values reach a tool through its environment,
        // which is never logged.
        tracing::debug!(program = %spec.program, args = ?spec.args, "starting a process");
        let mut cmd = self.command(spec).map_err(io)?;
        let child = cmd.spawn().map_err(|e| spawned(&spec.program, e))?;
        // Declared before the tree, so dropped after it: the tree's own stop runs first.
        let _running = child.id().map(|pid| Running::enter(&self.running, pid));
        let recorded = spec.tools.as_deref().zip(child.id()).and_then(|(dir, pid)| ledger::Recorded::write(dir, pid));
        let mut tree = Tree {
            child: Some(child),
            stopping: Arc::clone(&self.stopping),
            recorded,
        };
        let status = match tree.child.as_deref_mut() {
            Some(started) => lines::forward_lines(started, on_line).await.map_err(io)?,
            None => return Err(io(std::io::Error::other("the process went away"))),
        };
        // It exited on its own: what it left running is its business.
        tree.child = None;
        tracing::debug!(program = %spec.program, code = ?status.code(), "process exited");
        if shim::missing_behind_cmd(&spec.program, status.code()) {
            return Err(ProcessError::Missing { program: spec.program.clone() });
        }
        Ok(Exit { code: status.code() })
    }

    /// The command for `spec`: its own group or job, its output piped, headless.
    fn command(&self, spec: &ProcessSpec) -> std::io::Result<CommandWrap> {
        let stdin = match &spec.stdin {
            Some(path) => Stdio::from(std::fs::File::open(path)?),
            None => Stdio::null(),
        };
        let strip = self.strip_engine_overrides && shim::uses_engine(&spec.program);
        let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
        let withheld = shim::withheld(&spec.program, inherited);
        let (program, args) = wrapped(spec);
        let mut cmd = CommandWrap::with_new(program, |c: &mut Command| {
            c.args(args).stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
            // Before the spec's own: what the run delivers is set again after.
            for var in &withheld {
                c.env_remove(var);
            }
            c.envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
            if let Some(dir) = &spec.cwd {
                c.current_dir(dir);
            }
            if strip {
                for var in shim::ENGINE_OVERRIDES {
                    c.env_remove(var);
                }
            }
        });
        group(&mut cmd);
        Ok(cmd)
    }
}

impl ProcessRunner for SystemProcesses {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(self.run_to_end(spec, on_line))
    }
}

/// A spawned tool's tree, stopped when dropped before the tool exited (a timeout, a
/// cancellation), with its entry in the run's tools when it was recorded: removed once the tool
/// ended.
struct Tree {
    /// The process, until it is waited for.
    child: Option<Box<dyn ChildWrapper>>,
    /// What stops it (and its group) when it is dropped unwaited.
    stopping: Arc<Stopping>,
    /// Its ledger entry, taken by the stop.
    recorded: Option<ledger::Recorded>,
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            self.stopping.stop(child, self.recorded.take());
        }
    }
}

/// A tool in [`SystemProcesses`]'s running set while this lives.
struct Running(Arc<std::sync::Mutex<std::collections::BTreeSet<u32>>>, u32);

impl Running {
    fn enter(set: &Arc<std::sync::Mutex<std::collections::BTreeSet<u32>>>, pid: u32) -> Running {
        set.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(pid);
        Running(Arc::clone(set), pid)
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&self.1);
    }
}

/// Kills the process group `pid` leads, at once.
#[cfg(unix)]
fn kill_tree(pid: u32) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Some(group) = i32::try_from(pid).ok().filter(|p| *p > 1).map(Pid::from_raw)
        && let Err(e) = killpg(group, Signal::SIGKILL)
    {
        tracing::debug!(pid, error = %e, "a running tool's process group had ended");
    }
}

/// Kills process `pid` and its tree (`taskkill /T /F`), waiting for taskkill.
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt as _;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    let ended = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .status();
    if let Err(e) = ended {
        tracing::warn!(pid, error = %e, "taskkill couldn't run to end a running tool");
    }
}

/// The program and arguments to start: a Windows `.cmd` tool through `cmd /C`.
fn wrapped(spec: &ProcessSpec) -> (&str, Vec<&str>) {
    let args = spec.args.iter().map(String::as_str);
    if cfg!(windows) && shim::SHIMMED.contains(&spec.program.as_str()) {
        return ("cmd", ["/C", spec.program.as_str()].into_iter().chain(args).collect());
    }
    (spec.program.as_str(), args.collect())
}

#[cfg(unix)]
fn group(cmd: &mut CommandWrap) {
    cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
}

#[cfg(windows)]
fn group(cmd: &mut CommandWrap) {
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    // Headless: a status poll would otherwise flash a console window every few seconds.
    cmd.wrap(process_wrap::tokio::CreationFlags(CREATE_NO_WINDOW));
    cmd.wrap(process_wrap::tokio::JobObject);
}

/// The error of a tool that couldn't start: missing, or the OS's reason.
fn spawned(program: &str, e: std::io::Error) -> ProcessError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ProcessError::Missing { program: program.to_owned() },
        _ => ProcessError::Io {
            program: program.to_owned(),
            source: e,
        },
    }
}

#[cfg(test)]
#[path = "system_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
mod end_all_tests {
    use super::*;

    #[tokio::test]
    async fn end_all_now_ends_a_tool_still_running() {
        let processes = SystemProcesses::default();
        let spec = ProcessSpec::new("sleep", ["30"]);
        let p = processes.clone();
        let run = tokio::spawn(async move { p.run(&spec, &mut |_, _| {}).await });
        for _ in 0..50 {
            if !processes.running.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!processes.running.lock().unwrap().is_empty(), "the tool is running");
        processes.end_all_now();
        let exit = tokio::time::timeout(std::time::Duration::from_secs(5), run)
            .await
            .expect("ended at once")
            .unwrap()
            .unwrap();
        assert_ne!(exit.code, Some(0), "killed, not finished");
    }
}
