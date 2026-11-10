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

/// How it ended, as a failure's reason says it: `exited with code 1`, `exited with a signal`.
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.code {
            Some(code) => write!(f, "exited with code {code}"),
            None => f.write_str("exited with a signal"),
        }
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

/// How many of its last output lines stand in for a failed tool's silent standard error.
pub(crate) const STDOUT_REASON_LINES: usize = 5;

/// What a process printed, kept: its standard output, and its last standard error lines.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Captured {
    /// How it ended.
    pub exit: Exit,
    /// Its standard output, one line each.
    pub stdout: Vec<String>,
    /// Its last standard error lines (a Ruby backtrace's frames and known-harmless warnings left
    /// out).
    pub stderr: Vec<String>,
}

impl Captured {
    /// Whether it exited 0 ([`Exit::success`]).
    #[must_use]
    pub fn success(&self) -> bool {
        self.exit.success()
    }

    /// Why it failed, the one wording every failed tool's error uses: the error it printed as a
    /// machine-readable record on its standard output (Vagrant's `error-exit`, with
    /// `--machine-readable`), else its last error lines, else its last output lines (a tool, or a
    /// cloud image refusing a login, may print its refusal there), with how it ended (`denied
    /// (exited with code 1)`, `exited with a signal`).
    #[must_use]
    pub fn reason(&self) -> String {
        let cause = crate::tool_output::vagrant_error(&self.stdout).unwrap_or_else(|| {
            let quiet = self.stderr.iter().all(|l| l.trim().is_empty());
            let said = if quiet {
                self.stdout.get(self.stdout.len().saturating_sub(STDOUT_REASON_LINES)..).unwrap_or_default()
            } else {
                &self.stderr[..]
            };
            said.join("\n")
        });
        if cause.trim().is_empty() {
            self.exit.to_string()
        } else {
            format!("{} ({})", cause.trim(), self.exit)
        }
    }

    /// Its standard output when it exited 0, else why it failed ([`Captured::reason`]).
    ///
    /// # Errors
    ///
    /// It didn't exit 0: the error is its reason.
    pub fn output(&self) -> Result<String, String> {
        if self.success() { Ok(self.stdout.join("\n")) } else { Err(self.reason()) }
    }
}

/// Runs `spec`, keeping what it prints.
///
/// # Errors
///
/// The process can't start (missing, refused, an I/O error).
pub async fn capture(runner: &dyn ProcessRunner, spec: &ProcessSpec) -> Result<Captured, ProcessError> {
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

    fn reason(out: &Captured) -> String {
        out.reason()
    }

    fn ended(code: Option<i32>, stderr: &[&str], stdout: &[&str]) -> Captured {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Captured {
            exit: Exit::new(code),
            stdout: lines(stdout),
            stderr: lines(stderr),
        }
    }

    #[test]
    fn a_failure_says_why_from_stderr_else_the_end_of_stdout() {
        assert_eq!(reason(&ended(Some(1), &["denied"], &["noise"])), "denied (exited with code 1)");
        // Debian on AWS refuses root on stdout, with nothing on stderr.
        let refused = ended(Some(1), &["", " "], &["Please login as the user \"admin\" rather than the user \"root\"."]);
        assert_eq!(
            reason(&refused),
            "Please login as the user \"admin\" rather than the user \"root\". (exited with code 1)"
        );
        let long: Vec<String> = (1..=9).map(|n| format!("line {n}")).collect();
        let long: Vec<&str> = long.iter().map(String::as_str).collect();
        assert_eq!(
            reason(&ended(Some(2), &[], &long)),
            "line 5\nline 6\nline 7\nline 8\nline 9 (exited with code 2)"
        );
        assert_eq!(reason(&ended(None, &[], &[])), "exited with a signal");
    }

    /// Regression: on libvirt, a `vagrant status --machine-readable` refused for another
    /// process's lock failed with only fog's warnings on standard error, which stood in for the
    /// error Vagrant printed on its standard output.
    #[test]
    fn a_machine_readable_vagrant_error_is_the_reason() {
        let said = "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant \
                    process is currently reading or modifying\\nthe machine.";
        let ui = format!("1794342958,,ui,error,{said}");
        let exit = format!("1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}");
        let fog = "[fog][WARNING] Unrecognized arguments: libvirt_ip_command";
        let want = "Vagrant can't use the requested machine because it is locked! This\nmeans that another Vagrant \
                    process is currently reading or modifying\nthe machine. (Vagrant::Errors::MachineLocked) (exited with code 1)";
        assert_eq!(reason(&ended(Some(1), &[fog, fog, fog], &["1,web,state,running", &ui, &exit])), want);
        assert_eq!(reason(&ended(Some(1), &[], &[&ui, &exit])), want);
        // A Ruby error on standard error yields to Vagrant's own record of it.
        assert_eq!(reason(&ended(Some(1), &["Traceback"], &[&exit])), want);
    }

    #[test]
    fn a_silent_failure_reports_its_exit_status() {
        assert_eq!(reason(&ended(Some(1), &[], &[])), "exited with code 1");
        assert_eq!(reason(&ended(None, &[" "], &[])), "exited with a signal");
        assert_eq!(reason(&ended(Some(2), &["no space left"], &[])), "no space left (exited with code 2)");
        assert_eq!(ended(Some(0), &["warning"], &["a", "b"]).output(), Ok("a\nb".to_owned()));
        assert_eq!(ended(Some(3), &["bad"], &["a"]).output(), Err("bad (exited with code 3)".to_owned()));
    }

    #[test]
    fn a_spec_reads_as_its_command_line() {
        let spec = ProcessSpec::new("docker", ["compose", "ps"]);
        assert_eq!(spec.line(), "docker compose ps");
        assert!(Exit { code: Some(0) }.success() && !Exit { code: None }.success());
    }
}
