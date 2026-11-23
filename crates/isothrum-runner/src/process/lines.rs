//! Reading a running process's lines as they come.

use std::io;
use std::process::ExitStatus;
use std::time::{Duration, Instant};

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
    for (stream, lines) in [(Stream::Stdout, &mut out.lines), (Stream::Stderr, &mut err.lines)] {
        if let Some(line) = lines.finish() {
            on_line(stream, &line);
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
    /// Kept across reads: reading is cancel safe only because a partial line stays here.
    lines: Lines,
    done: bool,
}

impl<R: AsyncRead + Unpin> Reader<R> {
    fn new(inner: R) -> Reader<R> {
        Reader {
            inner: BufReader::new(inner),
            lines: Lines::default(),
            done: false,
        }
    }

    /// The next line, or `None` once the output closed (marking it done).
    async fn next(&mut self) -> io::Result<Option<String>> {
        loop {
            let bytes = self.inner.fill_buf().await?;
            if bytes.is_empty() {
                // At the end, a last line with no line break may still be held.
                self.done = true;
                return Ok(self.lines.finish());
            }
            let (used, line) = self.lines.feed(bytes, Instant::now());
            self.inner.consume(used);
            if line.is_some() {
                return Ok(line);
            }
        }
    }
}

/// How often a line a tool redraws in place is handed on, at most: a progress meter redrawn many
/// times a second would otherwise fill the event log.
const REDRAW_EVERY: Duration = Duration::from_secs(1);

/// A tool's output cut into lines as a terminal shows them.
///
/// A line ends at `\n`, at `\r\n`, or at a lone `\r`: a progress meter (`ovftool`'s `Disk
/// progress: 8%`, a download's percentage) redraws its line after a carriage return and prints no
/// `\n` until it is done, so only splitting there shows it as it moves. A carriage return with
/// nothing before it moves no text and ends no line, except as part of `\r\n` (a blank line).
///
/// A line ended by a lone `\r` is a redraw, handed on only when its text changed and at least
/// [`REDRAW_EVERY`] passed since the last redraw handed on; a line ended by `\n` or `\r\n` always
/// is, and the next redraw after it is too. Bytes that aren't UTF-8 are replaced: a line is decoded
/// whole, so a character is never cut at a read's boundary.
#[derive(Debug, Default)]
pub(in crate::process) struct Lines {
    /// The line read so far, without its line break.
    partial: Vec<u8>,
    /// What the last `\r` ended, until the byte after it says whether it was `\r\n`.
    after_cr: AfterCr,
    /// The last redraw handed on since the last whole line, and when.
    redrawn: Option<(Instant, String)>,
}

/// What the last `\r` ended.
#[derive(Debug, Default)]
enum AfterCr {
    /// No line: the last line break wasn't a `\r`, or it had nothing before it.
    #[default]
    Nothing,
    /// A line already handed on.
    Shown,
    /// A redraw held back: handed on after all if a `\n` follows (a whole line), else dropped.
    Held(String),
}

impl Lines {
    /// Reads `bytes` as far as the end of the next line to hand on: how many of them it took, and
    /// that line, or all of them and none when no line to hand on ended in them.
    pub(in crate::process) fn feed(&mut self, bytes: &[u8], now: Instant) -> (usize, Option<String>) {
        let mut rest = bytes;
        let used = |left: &[u8]| bytes.len() - left.len();
        while let Some((&first, after)) = rest.split_first() {
            if !matches!(self.after_cr, AfterCr::Nothing) {
                match first {
                    // `\r\n`: a whole line after all.
                    b'\n' => {
                        rest = after;
                        self.redrawn = None;
                        if let AfterCr::Held(line) = std::mem::take(&mut self.after_cr) {
                            return (used(rest), Some(line));
                        }
                        continue;
                    }
                    // `\r\r`, `\r\r\n`: still the same line break.
                    b'\r' => {
                        rest = after;
                        continue;
                    }
                    _ => self.after_cr = AfterCr::Nothing,
                }
            }
            let Some((text, ending, beyond)) = first_break(rest) else {
                self.partial.extend_from_slice(rest);
                return (bytes.len(), None);
            };
            self.partial.extend_from_slice(text);
            rest = beyond;
            if ending == b'\n' {
                self.redrawn = None;
                return (used(rest), Some(self.take()));
            }
            if self.partial.is_empty() {
                continue;
            }
            let line = self.take();
            if self.redraw_shown(&line, now) {
                self.after_cr = AfterCr::Shown;
                return (used(rest), Some(line));
            }
            self.after_cr = AfterCr::Held(line);
        }
        (bytes.len(), None)
    }

    /// What is left once the output ended (or stopped being read), as a last line: the line read
    /// so far, else a redraw held back (the meter's last state).
    pub(in crate::process) fn finish(&mut self) -> Option<String> {
        let held = match std::mem::take(&mut self.after_cr) {
            AfterCr::Held(line) => Some(line),
            AfterCr::Nothing | AfterCr::Shown => None,
        };
        if self.partial.is_empty() { held } else { Some(self.take()) }
    }

    /// Whether the redrawn `line`, ended `now`, is handed on (and if so, remembered).
    fn redraw_shown(&mut self, line: &str, now: Instant) -> bool {
        let shown = match &self.redrawn {
            None => true,
            Some((at, last)) => last != line && now.duration_since(*at) >= REDRAW_EVERY,
        };
        if shown {
            self.redrawn = Some((now, line.to_owned()));
        }
        shown
    }

    /// The line read so far, decoded lossily, leaving none.
    fn take(&mut self) -> String {
        let line = String::from_utf8_lossy(&self.partial).into_owned();
        self.partial.clear();
        line
    }
}

/// `bytes` cut at their first line break (`\n` or `\r`): what is before it, the break, and what
/// is after it.
fn first_break(bytes: &[u8]) -> Option<(&[u8], u8, &[u8])> {
    let end = bytes.iter().position(|b| matches!(b, b'\n' | b'\r'))?;
    let (text, tail) = bytes.split_at(end);
    let (&ending, after) = tail.split_first()?;
    Some((text, ending, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines `chunks` hand on, each chunk read `apart` after the one before.
    fn split(chunks: &[&[u8]], apart: Duration) -> Vec<String> {
        let (mut lines, mut out, mut now) = (Lines::default(), Vec::new(), Instant::now());
        for chunk in chunks {
            let mut rest = *chunk;
            while !rest.is_empty() {
                let (used, line) = lines.feed(rest, now);
                rest = &rest[used..];
                out.extend(line);
            }
            now += apart;
        }
        out.extend(lines.finish());
        out
    }

    /// `text` read one byte at a time, all at once.
    fn bytewise(text: &[u8]) -> Vec<String> {
        split(&text.chunks(1).collect::<Vec<_>>(), Duration::ZERO)
    }

    #[test]
    fn a_lone_carriage_return_ends_a_line_and_crlf_is_one_line_break() {
        for chunk in 1..8 {
            let text = b"a\r\nb\n\r\n\nc\r\r\nd\re\n\rf";
            let read: Vec<&[u8]> = text.chunks(chunk).collect();
            assert_eq!(split(&read, Duration::ZERO), ["a", "b", "", "", "c", "d", "e", "f"], "{chunk} bytes a read");
        }
        assert_eq!(bytewise(b"\r\n\r\n"), ["", ""], "blank CRLF lines are kept");
        assert_eq!(bytewise(b"\rlast"), ["last"], "a carriage return with nothing before it ends nothing");
        assert_eq!(bytewise(b"\n"), [""]);
        assert_eq!(bytewise(b""), Vec::<String>::new());
    }

    #[test]
    fn whole_crlf_lines_are_never_held_back_as_redraws() {
        let text = "x\r\n".repeat(50) + "50%\rdone\r\n";
        let mut expected = vec!["x"; 50];
        expected.extend(["50%", "done"]);
        assert_eq!(bytewise(text.as_bytes()), expected);
    }

    #[test]
    fn a_progress_meter_redrawn_in_place_is_handed_on_as_it_moves() {
        // What `ovftool` prints to a pipe, byte for byte: each percentage after a carriage return,
        // a `\n` only once it is done.
        let mut text = b"Writing OVA package: out/test.ova\n".to_vec();
        for percent in 1..=3 {
            text.extend(format!("\rDisk progress: {percent}%").bytes());
        }
        text.extend(b"\rTransfer Completed    \nCompleted successfully\n");
        let read: Vec<&[u8]> = text.split_inclusive(|b| *b == b'%').collect();
        assert_eq!(
            split(&read, Duration::from_secs(5)),
            [
                "Writing OVA package: out/test.ova",
                "Disk progress: 1%",
                "Disk progress: 2%",
                "Disk progress: 3%",
                "Transfer Completed    ",
                "Completed successfully",
            ]
        );
    }

    #[test]
    fn redraws_come_through_at_most_once_a_second_and_only_when_they_change() {
        let at = |secs: f64, text: &str| (Duration::from_secs_f64(secs), text.to_owned());
        let shown = |redraws: &[(Duration, String)]| {
            let (mut lines, start, mut out) = (Lines::default(), Instant::now(), Vec::new());
            for (after, text) in redraws {
                out.extend(lines.feed(text.as_bytes(), start + *after).1);
            }
            out
        };
        let busy: Vec<_> = (0..30).map(|i| at(f64::from(i) * 0.1, &format!("{i}%\r"))).collect();
        assert_eq!(shown(&busy), ["0%", "10%", "20%"], "ten a second, one shown a second");
        let stuck = [at(0.0, "waiting\r"), at(1.5, "waiting\r"), at(3.0, "waiting\r"), at(3.1, "ready\r")];
        assert_eq!(shown(&stuck), ["waiting", "ready"], "the same text again is not shown again");
        let after_line = [at(0.0, "1%\r"), at(0.2, "copied\n"), at(0.3, "2%\r")];
        assert_eq!(shown(&after_line), ["1%", "copied", "2%"], "a whole line lets the next redraw through");
    }

    #[test]
    fn a_redraw_held_back_is_the_last_line_when_the_output_ends_on_it() {
        assert_eq!(bytewise(b"1%\r2%\r3%\r"), ["1%", "3%"]);
        assert_eq!(bytewise(b"1%\r2%\r3%\rdone"), ["1%", "done"]);
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
        // A two-byte buffer makes every multi-byte character span reads: a line is gathered whole
        // first, so a character is never cut at a chunk boundary.
        let mut input = "a\r\nb\rc\n\u{20ac}\u{20ac}\n".as_bytes().to_vec();
        input.extend_from_slice(&[0xff, b'd']);
        let mut reader = Reader {
            inner: BufReader::with_capacity(2, &input[..]),
            lines: Lines::default(),
            done: false,
        };
        let mut lines = Vec::new();
        while let Some(line) = reader.next().await.unwrap() {
            lines.push(line);
        }
        assert_eq!(lines, ["a", "b", "c", "\u{20ac}\u{20ac}", "\u{fffd}d"]);
        assert!(reader.done);
    }

    #[tokio::test]
    async fn a_redraw_is_read_while_the_tool_is_still_writing_its_line() {
        // The tool's pipe stays open, with no `\n` in it: reading up to one never returned.
        let (mut tool, pipe) = tokio::io::duplex(64);
        tokio::io::AsyncWriteExt::write_all(&mut tool, b"\rDisk progress: 8%\rDisk progress: 9")
            .await
            .unwrap();
        let mut reader = Reader::new(pipe);
        let read = tokio::time::timeout(Duration::from_secs(5), reader.next()).await;
        assert_eq!(read.expect("read before the tool ends its line").unwrap().as_deref(), Some("Disk progress: 8%"));
        drop(tool);
        assert_eq!(reader.next().await.unwrap().as_deref(), Some("Disk progress: 9"));
        assert_eq!(reader.next().await.unwrap(), None);
    }
}
