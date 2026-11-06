//! Running a tool and reading its output, with an optional time limit. The launcher's
//! `platform::exec` did this on its own process layer (with `unsafe` for process groups and UAC);
//! here it is thin convenience over the Isothrum runner's no-unsafe [`ProcessRunner`], which
//! already puts each tool in its own process group (or Windows job object) so a dropped timeout
//! ends the whole tree it started.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use isothrum_runner::{ProcessRunner, ProcessSpec, Stream};

use crate::error::{Error, Result};

/// Runs `program args` in `cwd` and returns everything it printed to standard output, whatever its
/// exit code: a probe reads the output of a tool that may well exit non-zero.
///
/// # Errors
///
/// [`Error::Process`] when the tool is not installed or the OS refuses to start it.
pub async fn read(runner: &dyn ProcessRunner, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    Ok(collect(runner, &spec(program, args, cwd)).await?.out)
}

/// Runs `program args` in `cwd` and returns its standard output, failing when it exits non-zero:
/// the caller needed the tool to succeed, not only to say something.
///
/// # Errors
///
/// [`Error::Process`] when the tool can't be started, [`Error::Exit`] when it exits non-zero.
pub async fn run(runner: &dyn ProcessRunner, program: &str, args: &[&str], cwd: Option<&Path>) -> Result<String> {
    let s = spec(program, args, cwd);
    let done = collect(runner, &s).await?;
    if done.ok {
        return Ok(done.out);
    }
    let reason = done.err.lines().last().or_else(|| done.out.lines().last()).unwrap_or("exited non-zero").to_owned();
    Err(Error::Exit { command: s.line(), reason })
}

/// Awaits `future` with a time limit: past `limit` it is dropped (the runner then ends the process
/// it started) and [`Error::Timeout`] is returned.
///
/// # Errors
///
/// `what`'s own error, or [`Error::Timeout`] when `limit` passes first.
pub async fn within<T>(what: &str, limit: Duration, future: impl Future<Output = Result<T>>) -> Result<T> {
    match tokio::time::timeout(limit, future).await {
        Ok(result) => result,
        Err(_elapsed) => Err(Error::Timeout { what: what.to_owned(), limit }),
    }
}

/// `program` with `args`, run in `cwd`, nothing else set.
fn spec(program: &str, args: &[&str], cwd: Option<&Path>) -> ProcessSpec {
    let mut s = ProcessSpec::new(program, args.iter().copied());
    s.cwd = cwd.map(Path::to_path_buf);
    s
}

/// What a finished process left: its standard output, its standard error, and whether it exited
/// with zero.
struct Done {
    out: String,
    err: String,
    ok: bool,
}

/// Runs `spec`, collecting its output line by line.
async fn collect(runner: &dyn ProcessRunner, spec: &ProcessSpec) -> Result<Done> {
    let mut out = String::new();
    let mut err = String::new();
    let exit = runner
        .run(spec, &mut |stream, line| {
            let buffer = if stream == Stream::Stdout { &mut out } else { &mut err };
            buffer.push_str(line);
            buffer.push('\n');
        })
        .await?;
    Ok(Done { out, err, ok: exit.success() })
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    #[tokio::test]
    async fn read_returns_the_output_whatever_the_exit() {
        let fake = FakeProcesses::rules(&[("tool one", Reply::ok("line-a\nline-b"))]);
        let out = read(&fake, "tool", &["one"], None).await.unwrap();
        assert!(out.contains("line-a") && out.contains("line-b"), "{out}");
    }

    #[tokio::test]
    async fn run_fails_with_the_command_and_its_reason_on_a_non_zero_exit() {
        let fake = FakeProcesses::rules(&[("tool go", Reply::fail(2, "the reason"))]);
        let err = run(&fake, "tool", &["go"], None).await.unwrap_err();
        let Error::Exit { command, reason } = err else { panic!("{err:?}") };
        assert_eq!(command, "tool go");
        assert_eq!(reason, "the reason");
    }

    #[tokio::test]
    async fn within_returns_a_timeout_once_the_limit_passes() {
        let never = std::future::pending::<Result<()>>();
        let outcome = within("a slow thing", Duration::from_millis(10), never).await;
        assert!(matches!(outcome, Err(Error::Timeout { .. })), "{outcome:?}");
    }

    #[tokio::test]
    async fn within_passes_a_quick_result_through() {
        let quick = async { Ok::<_, Error>(7) };
        assert_eq!(within("x", Duration::from_secs(5), quick).await.unwrap(), 7);
    }
}
