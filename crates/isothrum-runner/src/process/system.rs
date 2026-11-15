//! Real processes: each tool in its own process group (unix, through `process-wrap`) or created
//! inside a kill-on-close job object of its own (Windows, [`super::job`]), so a timeout, a
//! cancellation or this process's death ends the whole tree it started (`vagrant` is a wrapper
//! around `ruby`, `cmd /C az` around `python`), headless on Windows. A
//! tool cut short is asked to stop first (see [`super::stopping`]). Each tool speaks in the C
//! locale ([`super::locale`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use process_wrap::tokio::ChildWrapper;
#[cfg(unix)]
use process_wrap::tokio::CommandWrap;
#[cfg(unix)]
use tokio::process::Command;

use super::stopping::{GRACE, Removal, Stopping};
use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, ledger, lines, locale, shim};
use crate::BoxFuture;

/// Starts real processes. [`SystemProcesses::default`] reads the Docker CLI's config
/// (`$DOCKER_CONFIG/config.json`, else `~/.docker/config.json`) to learn whether the user picked
/// an engine context of their own.
///
/// A tool cut short (its operation's future dropped: a cancel, a step's timeout) is asked to stop
/// (SIGINT to its process group on Unix, Ctrl-C on its console on Windows, so Terraform saves its
/// state) and ended once it has exited, or 30 seconds later. Its clones share the tools
/// stopping: a process about to exit waits for them with [`SystemProcesses::stopped`].
#[derive(Debug, Clone)]
pub struct SystemProcesses {
    /// Drop `DOCKER_HOST` and `DOCKER_CONTEXT` for `docker` (the user picked a context).
    strip_engine_overrides: bool,
    /// The tools cut short that are still stopping.
    stopping: Arc<Stopping>,
    /// The process groups (unix) or trees (Windows) of the tools running now, by leader pid, each
    /// with the removal of the container it runs in, if any.
    running: Arc<std::sync::Mutex<RunningSet>>,
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
        for (pid, removal) in running {
            kill_tree(pid);
            if let Some(removal) = removal {
                removal.run();
            }
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
        let launch = self.launch(spec).map_err(io)?;
        let child = start(launch).map_err(|e| spawned(spec, e))?;
        // Declared before the tree, so dropped after it: the tree's own stop runs first.
        let removal = self.removal(spec);
        let _running = child.id().map(|pid| Running::enter(&self.running, pid, removal.clone()));
        let recorded = spec
            .ledger_dir
            .as_deref()
            .zip(child.id())
            .and_then(|(dir, pid)| ledger::Recorded::write(dir, pid, spec.container.as_deref()));
        let mut tree = Tree {
            child: Some(child),
            stopping: Arc::clone(&self.stopping),
            recorded,
            removal,
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

    /// The removal of the container `spec`'s tool runs in, when it names one, with the
    /// environment the tool gets, so it reaches the same engine.
    fn removal(&self, spec: &ProcessSpec) -> Option<Removal> {
        let name = spec.container.as_deref()?;
        let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
        let withheld = shim::withheld(&spec.program, inherited);
        let strip = self.strip_engine_overrides && shim::uses_engine(&spec.program);
        let stripped = if strip {
            shim::ENGINE_OVERRIDES.iter().map(|v| (*v).to_owned()).collect()
        } else {
            Vec::new()
        };
        Some(Removal {
            program: spec.program.clone(),
            name: name.to_owned(),
            withheld,
            env: spec.env.clone(),
            stripped,
        })
    }

    /// What starting `spec`'s tool takes: its program and arguments (a shimmed tool through
    /// `cmd /C`), its input, and the changes to this process's environment it gets, in order.
    fn launch(&self, spec: &ProcessSpec) -> std::io::Result<Launch> {
        let stdin = spec.stdin.as_ref().map(std::fs::File::open).transpose()?;
        let strip = self.strip_engine_overrides && shim::uses_engine(&spec.program);
        let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
        // Before the spec's own: what the run delivers is set again after.
        let mut env: Vec<(OsString, Option<OsString>)> = shim::withheld(&spec.program, inherited).into_iter().map(|var| (var.into(), None)).collect();
        env.extend(
            locale::settings(|name| std::env::var_os(name))
                .into_iter()
                .map(|(var, value)| (var.into(), value)),
        );
        env.extend(spec.env.iter().map(|(k, v)| (k.into(), Some(v.into()))));
        if strip {
            env.extend(shim::ENGINE_OVERRIDES.iter().map(|var| ((*var).into(), None)));
        }
        let (program, args) = wrapped(spec);
        Ok(Launch {
            program: program.to_owned(),
            args: args.into_iter().map(str::to_owned).collect(),
            env,
            cwd: spec.cwd.clone(),
            stdin,
        })
    }
}

/// What starting a tool takes, whatever the OS.
#[derive(Debug)]
pub(super) struct Launch {
    /// The program, as started (`cmd` for a shimmed tool).
    pub(super) program: String,
    pub(super) args: Vec<String>,
    /// Changes to this process's environment, in order: a value sets the variable, none removes it.
    pub(super) env: Vec<(OsString, Option<OsString>)>,
    pub(super) cwd: Option<PathBuf>,
    /// Its input, else none (the null device).
    pub(super) stdin: Option<std::fs::File>,
}

/// Starts `launch` as the leader of a process group of its own, its output piped.
#[cfg(unix)]
#[expect(clippy::disallowed_methods, reason = "the process module's spawner: every tool call of the runner starts here")]
fn start(launch: Launch) -> std::io::Result<Box<dyn ChildWrapper>> {
    let stdin = launch.stdin.map_or_else(Stdio::null, Stdio::from);
    let mut cmd = CommandWrap::with_new(&launch.program, |c: &mut Command| {
        c.args(&launch.args).stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (var, value) in &launch.env {
            match value {
                Some(value) => c.env(var, value),
                None => c.env_remove(var),
            };
        }
        if let Some(dir) = &launch.cwd {
            c.current_dir(dir);
        }
    });
    cmd.wrap(process_wrap::tokio::ProcessGroup::leader());
    cmd.spawn()
}

/// Starts `launch` inside a kill-on-close job object of its own, headless, its output piped
/// ([`super::job`]).
#[cfg(windows)]
fn start(launch: Launch) -> std::io::Result<Box<dyn ChildWrapper>> {
    super::job::spawn(launch)
}

impl ProcessRunner for SystemProcesses {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(self.run_to_end(spec, on_line))
    }
}

/// A spawned tool's tree, stopped when dropped before the tool exited (a timeout, a
/// cancellation), with its entry in the run's tools when it was recorded (removed once the tool
/// ended) and the removal of the container it runs in, if any.
struct Tree {
    /// The process, until it is waited for.
    child: Option<Box<dyn ChildWrapper>>,
    /// What stops it (and its group) when it is dropped unwaited.
    stopping: Arc<Stopping>,
    /// Its ledger entry, taken by the stop.
    recorded: Option<ledger::Recorded>,
    /// The removal of the container it runs in, taken by the stop.
    removal: Option<Removal>,
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            self.stopping.stop(child, self.recorded.take(), self.removal.take());
        }
    }
}

/// The tools running now, by leader pid, each with the removal of its container, if any.
type RunningSet = std::collections::BTreeMap<u32, Option<Removal>>;

/// A tool in [`SystemProcesses`]'s running set while this lives.
struct Running(Arc<std::sync::Mutex<RunningSet>>, u32);

impl Running {
    fn enter(set: &Arc<std::sync::Mutex<RunningSet>>, pid: u32, removal: Option<Removal>) -> Running {
        set.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(pid, removal);
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
#[expect(clippy::disallowed_methods, reason = "the process module's own ending of a process tree")]
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

/// The error of the tool `spec` that couldn't start: missing, or the OS's reason. The OS refuses
/// a start in a working folder that isn't there with `NotFound` too: that is said of the folder,
/// not taken for a tool that isn't installed.
fn spawned(spec: &ProcessSpec, e: std::io::Error) -> ProcessError {
    let program = spec.program.clone();
    let folder_gone = spec.cwd.as_deref().filter(|dir| !dir.is_dir());
    match (e.kind(), folder_gone) {
        (std::io::ErrorKind::NotFound, Some(dir)) => ProcessError::Io {
            program,
            source: std::io::Error::new(std::io::ErrorKind::NotFound, format!("its working folder {} doesn't exist", dir.display())),
        },
        (std::io::ErrorKind::NotFound, None) => ProcessError::Missing { program },
        _ => ProcessError::Io { program, source: e },
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
