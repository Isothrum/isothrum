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
pub(crate) async fn read(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> Result<String, ProcessError> {
    run(runner, program, args).await.map(|out| out.stdout.join("\n"))
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

    #[tokio::test]
    async fn read_returns_the_output_whatever_the_exit() {
        let fake = FakeProcesses::rules(&[("tool one", Reply::ok("line-a\nline-b"))]);
        let out = read(&fake, "tool", &["one"]).await.unwrap();
        assert!(out.contains("line-a") && out.contains("line-b"), "{out}");

        let failing = FakeProcesses::rules(&[("tool two", Reply::fail(2, "the reason"))]);
        assert!(read(&failing, "tool", &["two"]).await.is_ok());
    }

    #[tokio::test]
    async fn a_failed_read_says_why_as_the_runner_does() {
        let said = FakeProcesses::rules(&[("tool", Reply::fail(1, "no such plugin"))]);
        assert_eq!(read_ok(&said, "tool", &[]).await, Err("no such plugin (exited with code 1)".to_owned()));
        let mut on_stdout = Reply::ok("refused: log in as admin");
        on_stdout.code = 2;
        let on_stdout = FakeProcesses::rules(&[("tool", on_stdout)]);
        assert_eq!(
            read_ok(&on_stdout, "tool", &[]).await,
            Err("refused: log in as admin (exited with code 2)".to_owned())
        );
        let silent = FakeProcesses::rules(&[("tool", Reply::fail(3, ""))]);
        assert_eq!(read_ok(&silent, "tool", &[]).await, Err("exited with code 3".to_owned()));
    }

    #[tokio::test]
    async fn read_fails_when_the_tool_is_missing() {
        let fake = FakeProcesses::rules(&[("tool", Reply::missing())]);
        assert!(read(&fake, "tool", &[]).await.is_err());
    }
}
