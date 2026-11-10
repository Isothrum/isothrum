//! Running the tools a manifest names: the [`ProcessRunner`] port. [`SystemProcesses`] starts
//! real processes (never through a shell, headless on Windows, each tool in a process group or
//! job object so a timeout or a cancellation ends the whole tree it started, after asking it to
//! stop on Unix); the `fake` feature's
//! `fake::FakeProcesses` answers from a test's script instead.

mod elevated;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
mod lines;
mod shim;
mod stopping;
mod system;
mod tail;

use std::path::PathBuf;

pub(crate) use elevated::Elevated;
pub use elevated::may_manage_hyperv;
pub(crate) use shim::TERRAFORM_PROGRAMS;
pub use system::SystemProcesses;
pub(crate) use tail::Tail;

use crate::BoxFuture;
use crate::events::Stream;

/// One process to start.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProcessSpec {
    /// The program: a bare tool name, found on PATH.
    pub program: String,
    /// Its arguments, passed as they are (no shell ever reads them).
    pub args: Vec<String>,
    /// Its working folder (default: the runner's).
    pub cwd: Option<PathBuf>,
    /// Variables added to the runner's environment.
    pub env: Vec<(String, String)>,
    /// A file piped into its standard input (default: none).
    pub stdin: Option<PathBuf>,
}

impl ProcessSpec {
    /// `program` with `args`, everything else at its default.
    pub fn new(program: &str, args: impl IntoIterator<Item = impl Into<String>>) -> ProcessSpec {
        ProcessSpec {
            program: program.to_owned(),
            args: args.into_iter().map(Into::into).collect(),
            ..ProcessSpec::default()
        }
    }

    /// The command as one line (`docker compose ps`), for messages and tests.
    #[must_use]
    pub fn line(&self) -> String {
        command_line(&self.program, &self.args)
    }

    /// The value it is given for variable `name` (the last, when it is given twice).
    #[must_use]
    pub fn env(&self, name: &str) -> Option<&str> {
        self.env.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// How a process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Exit {
    /// Its exit code, when it exited (not killed by a signal).
    pub code: Option<i32>,
}

impl Exit {
    /// A process that ended with `code` (`None`: killed by a signal).
    #[must_use]
    pub fn new(code: Option<i32>) -> Exit {
        Exit { code }
    }

    /// Whether it exited with 0.
    #[must_use]
    pub fn success(self) -> bool {
        self.code == Some(0)
    }
}

/// Why a process couldn't run to its end.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ProcessError {
    /// The program isn't on PATH.
    #[error("`{program}` isn't installed or isn't on PATH")]
    Missing {
        /// The program.
        program: String,
    },
    /// A call the way it would run can't carry as asked (an elevated call: an argument
    /// PowerShell would mangle, a standard input it can't be given).
    #[error("refusing to run {program}: {why}")]
    Refused {
        /// The program.
        program: String,
        /// Why.
        why: String,
    },
    /// It needed elevation (Hyper-V's), which couldn't be had: the prompt declined, the
    /// elevated session ended.
    #[error("elevation: {reason}")]
    Elevation {
        /// Why.
        reason: String,
    },
    /// Starting it, piping it or waiting for it failed.
    #[error("`{program}`: {source}")]
    Io {
        /// The program.
        program: String,
        /// The OS's reason.
        source: std::io::Error,
    },
}

/// Receives each line a process prints, with the output it came from.
pub type OnLine<'a> = &'a mut (dyn FnMut(Stream, &str) + Send);

/// Starts processes. Dropping the future of [`ProcessRunner::run`] before it resolves (a timeout,
/// a cancelled operation) ends the process and everything it started.
pub trait ProcessRunner: Send + Sync {
    /// Runs `spec` to its end, handing each line it prints to `on_line` (decoded lossily).
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>>;
}

/// `program` and `args` as one line, separated by spaces.
fn command_line(program: &str, args: &[String]) -> String {
    std::iter::once(program).chain(args.iter().map(String::as_str)).collect::<Vec<_>>().join(" ")
}

/// What a process printed, kept: its standard output, and its last standard error lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Captured {
    /// How it ended.
    pub(crate) exit: Exit,
    /// Its standard output, one line each.
    pub(crate) stdout: Vec<String>,
    /// Its last standard error lines (a Ruby backtrace's frames left out).
    pub(crate) stderr: Vec<String>,
}

impl Captured {
    /// Whether it exited 0.
    pub(crate) fn succeeded(&self) -> bool {
        self.exit.success()
    }
}

/// Runs `spec`, keeping what it prints; fails when the process can't start (missing, refused, an
/// I/O error).
pub(crate) async fn capture(runner: &dyn ProcessRunner, spec: &ProcessSpec) -> Result<Captured, ProcessError> {
    capture_echoing(runner, spec, &mut |_, _| {}).await
}

/// [`capture`], each line also handed to `echo` as it comes.
pub(crate) async fn capture_echoing(runner: &dyn ProcessRunner, spec: &ProcessSpec, echo: OnLine<'_>) -> Result<Captured, ProcessError> {
    let mut stdout = Vec::new();
    let mut tail = Tail::default();
    let mut keep = |stream: Stream, line: &str| {
        echo(stream, line);
        match stream {
            Stream::Stdout => stdout.push(line.to_owned()),
            Stream::Stderr => tail.push(line),
        }
    };
    let exit = runner.run(spec, &mut keep).await?;
    Ok(Captured {
        exit,
        stdout,
        stderr: tail.into_lines(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_spec_reads_as_its_command_line() {
        let spec = ProcessSpec::new("docker", ["compose", "ps"]);
        assert_eq!(spec.line(), "docker compose ps");
        assert!(Exit { code: Some(0) }.success() && !Exit { code: None }.success());
    }
}
