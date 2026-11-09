//! Real processes, through `process-wrap` (no `unsafe` here): each tool in its own process group
//! (unix) or job object (Windows), so a timeout or a cancellation ends the whole tree it started
//! (`vagrant` is a wrapper around `ruby`, `cmd /C az` around `python`), headless on Windows.

use std::path::Path;
use std::process::Stdio;

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::process::Command;

use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, lines, shim};
use crate::BoxFuture;

/// Starts real processes. [`SystemProcesses::default`] reads the Docker CLI's config
/// (`$DOCKER_CONFIG/config.json`, else `~/.docker/config.json`) to learn whether the user picked
/// an engine context of their own.
#[derive(Debug, Clone)]
pub struct SystemProcesses {
    /// Drop `DOCKER_HOST` and `DOCKER_CONTEXT` for `docker` (the user picked a context).
    strip_engine_overrides: bool,
}

impl Default for SystemProcesses {
    fn default() -> SystemProcesses {
        SystemProcesses::for_host(|v| std::env::var(v).ok(), std::env::home_dir().as_deref())
    }
}

impl SystemProcesses {
    /// Real processes, the Docker CLI's config read from `$DOCKER_CONFIG/config.json`, else
    /// `<home>/.docker/config.json` (`var` reads the environment, `home` is the user's): it says
    /// whether the user picked an engine context of their own.
    fn for_host(var: impl Fn(&str) -> Option<String>, home: Option<&Path>) -> SystemProcesses {
        let dir = var("DOCKER_CONFIG").map(std::path::PathBuf::from).or_else(|| home.map(|h| h.join(".docker")));
        let config = dir.and_then(|d| std::fs::read_to_string(d.join("config.json")).ok());
        SystemProcesses {
            strip_engine_overrides: shim::strips_engine_overrides(config.as_deref()),
        }
    }

    async fn run_to_end(&self, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(arg) = shim::refused_arg(&spec.program, &spec.args) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                arg: arg.escape_debug().to_string(),
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
        let mut tree = Tree(Some(child));
        let status = match tree.0.as_deref_mut() {
            Some(started) => lines::forward_lines(started, on_line).await.map_err(io)?,
            None => return Err(io(std::io::Error::other("the process went away"))),
        };
        // It exited on its own: what it left running is its business.
        tree.0 = None;
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

/// A spawned tool's tree, ended when dropped before the tool exited (a timeout, a cancellation).
struct Tree(Option<Box<dyn ChildWrapper>>);

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            // SIGKILL to the process group, or the job object terminated: nothing it started
            // outlives it. Already gone is fine.
            tracing::debug!(pid = ?child.id(), "ending a process tree cut short");
            if let Err(e) = child.start_kill() {
                tracing::debug!(error = %e, "the process tree had already ended");
            }
        }
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
