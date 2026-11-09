//! Running a tool and reading what it printed, over the Isothrum runner's no-unsafe
//! [`ProcessRunner`], which puts each tool in its own process group (or Windows job object) so a
//! dropped future ends the whole tree it started.

use isothrum_runner::process::ProcessError;
use isothrum_runner::{ProcessRunner, ProcessSpec, Stream};

/// Runs `program args` and returns everything it printed to standard output, whatever its exit
/// code: a probe reads the output of a tool that may well exit non-zero.
///
/// # Errors
///
/// [`ProcessError`] when the tool is not installed or the OS refuses to start it.
pub(crate) async fn read(
    runner: &dyn ProcessRunner,
    program: &str,
    args: &[&str],
) -> Result<String, ProcessError> {
    let spec = ProcessSpec::new(program, args.iter().copied());
    let mut out = String::new();
    runner
        .run(&spec, &mut |stream, line| {
            if stream == Stream::Stdout {
                out.push_str(line);
                out.push('\n');
            }
        })
        .await?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

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
    async fn read_fails_when_the_tool_is_missing() {
        let fake = FakeProcesses::rules(&[("tool", Reply::missing())]);
        assert!(read(&fake, "tool", &[]).await.is_err());
    }
}
