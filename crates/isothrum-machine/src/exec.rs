//! Running a tool and reading what it printed, over the Isothrum runner's no-unsafe
//! [`ProcessRunner`], which puts each tool in its own process group (or Windows job object) so a
//! dropped future ends the whole tree it started. Every probe has a time limit: a tool that runs
//! out of it is dropped (and ended by the runner), and said as timed out, not as missing.

use std::time::Duration;

use isothrum_runner::{Captured, ProcessError, ProcessRunner, ProcessSpec, capture};

/// Why a probe got no answer.
#[derive(Debug)]
pub(crate) enum Unanswered {
    /// The tool couldn't be started: not installed, or the OS refused.
    NotStarted(ProcessError),
    /// It ran past the probe's time limit.
    TimedOut(Duration),
}

impl Unanswered {
    /// Whether the tool is there: it started, and only ran out of time (a cold `VBoxSVC`).
    pub(crate) fn tool_is_there(&self) -> bool {
        matches!(self, Unanswered::TimedOut(_))
    }
}

impl std::fmt::Display for Unanswered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unanswered::NotStarted(e) => e.fmt(f),
            Unanswered::TimedOut(limit) => write!(f, "no answer within {} s", limit.as_secs()),
        }
    }
}

/// Runs `program args` for at most `limit` and returns how it ended, with what it printed,
/// whatever its exit code: a probe reads the output of a tool that may well exit non-zero.
///
/// # Errors
///
/// [`Unanswered`] when the tool couldn't be started or ran out of time.
pub(crate) async fn probe(runner: &dyn ProcessRunner, limit: Duration, program: &str, args: &[&str]) -> Result<Captured, Unanswered> {
    let spec = ProcessSpec::new(program, args.iter().copied());
    match tokio::time::timeout(limit, capture(runner, &spec)).await {
        Ok(Ok(captured)) => Ok(captured),
        Ok(Err(e)) => Err(Unanswered::NotStarted(e)),
        Err(_) => Err(Unanswered::TimedOut(limit)),
    }
}

/// Everything `captured` printed, standard output then its last standard error lines: a warning
/// a tool prints on either.
pub(crate) fn all_lines(captured: &Captured) -> String {
    captured.stdout.iter().chain(&captured.stderr_tail).cloned().collect::<Vec<_>>().join("\n")
}

/// Runs `program args` for at most `limit` and returns its standard output when it exits 0.
///
/// # Errors
///
/// Why it didn't answer: it couldn't be started, it ran out of time, or it failed (the runner's
/// one wording of a failed tool, [`Captured::reason`]).
pub(crate) async fn read_ok(runner: &dyn ProcessRunner, limit: Duration, program: &str, args: &[&str]) -> Result<String, String> {
    probe(runner, limit, program, args)
        .await
        .map_err(|e| e.to_string())?
        .output()
        .map_err(|failed| failed.to_string())
}

#[cfg(test)]
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;

    const LIMIT: Duration = Duration::from_secs(25);

    /// A tool's answer that prints `stdout` and exits `code`.
    fn exiting(code: i32, stdout: &str) -> Reply {
        let mut reply = Reply::ok(stdout);
        reply.code = code;
        reply
    }

    #[tokio::test(start_paused = true)]
    async fn a_probe_returns_the_output_whatever_the_exit() {
        let fake = FakeProcesses::expect(&[("tool one", Reply::ok("line-a\nline-b"))]);
        assert_eq!(probe(&fake, LIMIT, "tool", &["one"]).await.unwrap().stdout, ["line-a", "line-b"]);

        let mut warned = exiting(2, "partial");
        warned.stderr = vec!["WARNING: careful".to_owned()];
        let failing = FakeProcesses::expect(&[("tool two", warned)]);
        assert_eq!(all_lines(&probe(&failing, LIMIT, "tool", &["two"]).await.unwrap()), "partial\nWARNING: careful");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_says_why_as_the_runner_does() {
        let said = FakeProcesses::expect(&[("tool", Reply::fail(1, "no such plugin"))]);
        assert_eq!(read_ok(&said, LIMIT, "tool", &[]).await, Err("no such plugin (exited with code 1)".to_owned()));
        let on_stdout = FakeProcesses::expect(&[("tool", exiting(2, "refused: log in as admin"))]);
        assert_eq!(
            read_ok(&on_stdout, LIMIT, "tool", &[]).await,
            Err("refused: log in as admin (exited with code 2)".to_owned())
        );
        let silent = FakeProcesses::expect(&[("tool", Reply::fail(3, ""))]);
        assert_eq!(read_ok(&silent, LIMIT, "tool", &[]).await, Err("exited with code 3".to_owned()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_missing_tool_is_said_as_missing_and_isnt_there() {
        let fake = FakeProcesses::expect(&[("tool", Reply::missing())]);
        let err = probe(&fake, LIMIT, "tool", &[]).await.unwrap_err();
        assert!(
            matches!(err, Unanswered::NotStarted(ProcessError::Missing { ref program }) if program == "tool"),
            "{err:?}"
        );
        assert!(!err.tool_is_there());
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_past_the_limit_is_there_and_said_as_timed_out() {
        let err = probe(&crate::testing::Stuck, LIMIT, "VBoxManage", &["--version"]).await.unwrap_err();
        assert!(err.tool_is_there());
        assert_eq!(err.to_string(), "no answer within 25 s");
        assert_eq!(
            read_ok(&crate::testing::Stuck, LIMIT, "vagrant", &[]).await,
            Err("no answer within 25 s".to_owned())
        );
    }
}
