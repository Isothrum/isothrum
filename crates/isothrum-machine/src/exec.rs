//! Running a tool and reading what it printed, over the Isothrum runner's no-unsafe
//! [`ProcessRunner`], which puts each tool in its own process group (or Windows job object) so a
//! dropped future ends the whole tree it started.

use isothrum_runner::{Captured, ProcessError, ProcessRunner, ProcessSpec, capture};

/// Runs `program args` and returns everything it printed to standard output, whatever its exit
/// code: a probe reads the output of a tool that may well exit non-zero.
///
/// # Errors
///
/// [`ProcessError`] when the tool is not installed or the OS refuses to start it.
pub(crate) async fn read_any_exit(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Result<String, ProcessError> {
    run(runner, program, args).await.map(|out| out.stdout.join("\n"))
}

/// Runs `program args` and returns everything it printed, standard output then its last
/// standard error lines, whatever its exit code: a warning a tool prints on either.
///
/// # Errors
///
/// [`ProcessError`] when the tool is not installed or the OS refuses to start it.
pub(crate) async fn read_all(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Result<String, ProcessError> {
    run(runner, program, args)
        .await
        .map(|out| out.stdout.iter().chain(&out.stderr_tail).cloned().collect::<Vec<_>>().join("\n"))
}

/// Runs `program args` and returns its standard output when it exits 0.
///
/// # Errors
///
/// Why it didn't answer: it couldn't be started, or it failed (the runner's one wording of a
/// failed tool, [`Captured::reason`]).
pub(crate) async fn read_ok(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Result<String, String> {
    run(runner, program, args).await.map_err(|e| e.to_string())?.output()
}

/// How `program args` ended, with what it printed.
async fn run(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Result<Captured, ProcessError> {
    capture(runner, &ProcessSpec::new(program, args.iter().copied())).await
}

#[cfg(test)]
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;

    /// A tool's answer that prints `stdout` and exits `code`.
    fn exiting(code: i32, stdout: &str) -> Reply {
        let mut reply = Reply::ok(stdout);
        reply.code = code;
        reply
    }

    #[tokio::test(start_paused = true)]
    async fn read_any_exit_returns_the_output_whatever_the_exit() {
        let fake = FakeProcesses::expect(&[("tool one", Reply::ok("line-a\nline-b"))]);
        assert_eq!(read_any_exit(&fake, "tool", &["one"]).await.unwrap(), "line-a\nline-b");

        let failing = FakeProcesses::expect(&[("tool two", exiting(2, "partial"))]);
        assert_eq!(read_any_exit(&failing, "tool", &["two"]).await.unwrap(), "partial");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_read_says_why_as_the_runner_does() {
        let said = FakeProcesses::expect(&[("tool", Reply::fail(1, "no such plugin"))]);
        assert_eq!(read_ok(&said, "tool", &[]).await, Err("no such plugin (exited with code 1)".to_owned()));
        let on_stdout = FakeProcesses::expect(&[("tool", exiting(2, "refused: log in as admin"))]);
        assert_eq!(
            read_ok(&on_stdout, "tool", &[]).await,
            Err("refused: log in as admin (exited with code 2)".to_owned())
        );
        let silent = FakeProcesses::expect(&[("tool", Reply::fail(3, ""))]);
        assert_eq!(read_ok(&silent, "tool", &[]).await, Err("exited with code 3".to_owned()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_missing_tool_is_said_as_missing() {
        let fake = FakeProcesses::expect(&[("tool", Reply::missing())]);
        let err = read_any_exit(&fake, "tool", &[]).await.unwrap_err();
        assert!(matches!(err, ProcessError::Missing { ref program } if program == "tool"), "{err:?}");
    }
}
