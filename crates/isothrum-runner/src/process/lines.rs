//! Reading a running process's lines as they come.

use std::io;
use std::process::ExitStatus;
use std::time::Duration;

use process_wrap::tokio::ChildWrapper;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::OnLine;
use crate::events::Stream;

/// How long a process's pipes are still read once it has exited: its last lines are in them, but
/// a process it left running (an SSH `ControlMaster`, a Terraform provider plugin) can hold them
/// open for good, and waiting for them to close hung the run (and its lock) forever.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

/// Forwards a piped child's stdout and stderr lines to `on_line` as they arrive, until both close,
/// or until [`DRAIN_AFTER_EXIT`] after the child itself exited. Returns its exit status.
///
/// Lines are read as bytes and decoded lossily: tools print in the console's codepage, and a line
/// that isn't UTF-8 once failed the stream, which dropped (and so killed) a `terraform apply`.
pub(super) async fn forward_lines(child: &mut dyn ChildWrapper, on_line: OnLine<'_>) -> io::Result<ExitStatus> {
    let not_piped = || io::Error::other("the process's output wasn't piped");
    let stdout = child.stdout().take().ok_or_else(not_piped)?;
    let stderr = child.stderr().take().ok_or_else(not_piped)?;
    let mut out = Reader::new(stdout);
    let mut err = Reader::new(stderr);
    let mut exited: Option<ExitStatus> = None;
    // Armed once the child has exited; never polled before.
    let drain = tokio::time::sleep(DRAIN_AFTER_EXIT);
    tokio::pin!(drain);
    while !(out.done && err.done) {
        tokio::select! {
            read = out.next(), if !out.done => if let Some(line) = read? { on_line(Stream::Stdout, &line) },
            read = err.next(), if !err.done => if let Some(line) = read? { on_line(Stream::Stderr, &line) },
            // The tool itself, not its whole group or job (which waits for what it left running).
            status = child.inner_mut().wait(), if exited.is_none() => {
                exited = Some(status?);
                drain.as_mut().reset(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
            },
            () = &mut drain, if exited.is_some() => break,
        }
    }
    // Cut off by the drain: what was half read is still a line.
    for (stream, buf) in [(Stream::Stdout, &mut out.buf), (Stream::Stderr, &mut err.buf)] {
        if !buf.is_empty() {
            on_line(stream, &take_line(buf));
        }
    }
    match exited {
        Some(status) => Ok(status),
        None => child.inner_mut().wait().await,
    }
}

/// One output of the child, read a line at a time.
struct Reader<R> {
    inner: BufReader<R>,
    /// Kept across reads: `read_until` is cancel safe only because a partial line stays here.
    buf: Vec<u8>,
    done: bool,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    fn new(inner: R) -> Reader<R> {
        Reader {
            inner: BufReader::new(inner),
            buf: Vec::new(),
            done: false,
        }
    }

    /// The next line, or `None` once the output closed (marking it done).
    async fn next(&mut self) -> io::Result<Option<String>> {
        let read = self.inner.read_until(b'\n', &mut self.buf).await?;
        // At the end, a last line with no `\n` may already be in the buffer.
        if read == 0 && self.buf.is_empty() {
            self.done = true;
            return Ok(None);
        }
        Ok(Some(take_line(&mut self.buf)))
    }
}

/// The line read into `buf` (its `\n` or `\r\n` cut off, bytes that aren't UTF-8 replaced),
/// leaving `buf` empty for the next one.
pub(super) fn take_line(buf: &mut Vec<u8>) -> String {
    let line = buf.strip_suffix(b"\n").unwrap_or(buf);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let line = String::from_utf8_lossy(line).into_owned();
    buf.clear();
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_loses_its_ending_only() {
        let mut buf = b"a\r\r\n".to_vec();
        assert_eq!(take_line(&mut buf), "a\r");
        assert_eq!(buf, Vec::<u8>::new());
        assert_eq!(take_line(&mut b"last".to_vec()), "last");
        assert_eq!(take_line(&mut b"\n".to_vec()), "");
    }

    #[tokio::test]
    async fn a_child_whose_output_isnt_piped_is_an_error_not_a_panic() {
        // An `expect("piped stdout")` panicked here.
        let program = if cfg!(windows) { "cmd" } else { "true" };
        let mut cmd = process_wrap::tokio::CommandWrap::with_new(program, |c| {
            if cfg!(windows) {
                c.args(["/C", "exit 0"]);
            }
            c.stdout(std::process::Stdio::null());
        });
        let mut child = cmd.spawn().unwrap();
        let err = forward_lines(child.as_mut(), &mut |_, _| {}).await.unwrap_err();
        assert_eq!(err.to_string(), "the process's output wasn't piped");
        assert!(child.wait().await.unwrap().success());
    }

    #[tokio::test]
    async fn lines_are_read_lossily_whatever_the_read_boundaries() {
        // A two-byte buffer makes every multi-byte character span reads: `read_until` gathers the
        // whole line first, so a character is never cut at a chunk boundary.
        let mut input = "a\r\nb\rc\n\u{20ac}\u{20ac}\n".as_bytes().to_vec();
        input.extend_from_slice(&[0xff, b'd']);
        let mut reader = Reader {
            inner: BufReader::with_capacity(2, &input[..]),
            buf: Vec::new(),
            done: false,
        };
        let mut lines = Vec::new();
        while let Some(line) = reader.next().await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, ["a", "b\rc", "\u{20ac}\u{20ac}", "\u{fffd}d"]);
        assert!(reader.done);
    }
}
