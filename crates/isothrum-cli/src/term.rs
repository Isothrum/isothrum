//! Where the command line writes: events as text for a person (standard error) or as JSON lines
//! (standard output), and what a command answers (standard output). A run never stops for its
//! output, so a write that fails is kept, and said once the command is done; a reader that left
//! (`isothrum up --json | head -1`) is no error at all.

use std::io::{ErrorKind, Write};
use std::sync::{Mutex, PoisonError};

use isothrum_runner::{ErrorCode, Event, EventKind, Events, Op};

use crate::error::{Error, Result};
use crate::text::human;

/// Where the command line writes.
pub(crate) struct Term {
    out: Mutex<Box<dyn Write + Send>>,
    err: Mutex<Box<dyn Write + Send>>,
    /// The first write that failed, its reader still there.
    failed: Mutex<Option<std::io::Error>>,
}

impl Term {
    /// Standard output and standard error.
    pub(crate) fn std() -> Term {
        Term::new(Box::new(std::io::stdout()), Box::new(std::io::stderr()))
    }

    /// `out` and `err`.
    pub(crate) fn new(out: Box<dyn Write + Send>, err: Box<dyn Write + Send>) -> Term {
        Term {
            out: Mutex::new(out),
            err: Mutex::new(err),
            failed: Mutex::new(None),
        }
    }

    /// A line on standard output.
    pub(crate) fn out(&self, line: &str) {
        self.write(&self.out, line);
    }

    /// A line on standard error.
    pub(crate) fn err(&self, line: &str) {
        self.write(&self.err, line);
    }

    fn write(&self, stream: &Mutex<Box<dyn Write + Send>>, line: &str) {
        let mut w = stream.lock().unwrap_or_else(PoisonError::into_inner);
        if let Err(e) = writeln!(w, "{line}").and_then(|()| w.flush())
            && e.kind() != ErrorKind::BrokenPipe
        {
            self.failed.lock().unwrap_or_else(PoisonError::into_inner).get_or_insert(e);
        }
    }

    /// `event` as a JSON line on standard output.
    pub(crate) fn json(&self, event: &Event) {
        match serde_json::to_string(event) {
            Ok(text) => self.out(&text),
            Err(e) => self.err(&format!("warning: an event couldn't be written as JSON: {e}")),
        }
    }

    /// The first write that failed, once the command is done.
    ///
    /// # Errors
    ///
    /// [`Error::Output`], when a write failed for another reason than its reader leaving.
    pub(crate) fn finish(&self) -> Result<()> {
        match self.failed.lock().unwrap_or_else(PoisonError::into_inner).take() {
            Some(e) => Err(Error::Output(e)),
            None => Ok(()),
        }
    }
}

/// The kind of an `error` event the command line writes itself (no step failed, no timeout).
pub(crate) fn failure(code: ErrorCode, message: String) -> EventKind {
    EventKind::Error {
        code,
        message,
        step: None,
        timeout: None,
    }
}

/// Events as JSON lines on standard output.
pub(crate) struct Json<'a>(pub(crate) &'a Term);

impl Events for Json<'_> {
    fn emit(&self, event: &Event) {
        self.0.json(event);
    }
}

/// The last event a command wrote: a failure it reports itself (a cancelled command, one that
/// failed before its operation began) continues the stream, unless the operation already ended
/// it with its own `error`.
#[derive(Debug, Default)]
pub(crate) struct Trail(Mutex<Option<Last>>);

/// What the trail keeps of the last event.
#[derive(Debug, Clone)]
struct Last {
    seq: u64,
    run: Option<String>,
    op: Option<Op>,
    error: bool,
}

impl Trail {
    fn keep(&self, event: &Event) {
        let last = Last {
            seq: event.seq,
            run: event.run.clone(),
            op: event.op,
            error: matches!(event.kind, EventKind::Error { .. }),
        };
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(last);
    }

    fn last(&self) -> Option<Last> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Whether the last event was an `error`: the failure was said.
    pub(crate) fn ended_in_error(&self) -> bool {
        self.last().is_some_and(|l| l.error)
    }

    /// The event of `kind` that follows the last one: its run and operation, else `run` and `op`.
    pub(crate) fn next(&self, run: Option<String>, op: Option<Op>, kind: EventKind) -> Event {
        match self.last() {
            Some(last) => Event::new(last.seq + 1, last.run, last.op, kind),
            None => Event::new(1, run, op, kind),
        }
    }

    /// [`Trail::next`], kept as the last event: for a command that writes its own events.
    pub(crate) fn follow(&self, run: Option<String>, op: Option<Op>, kind: EventKind) -> Event {
        let event = self.next(run, op, kind);
        self.keep(&event);
        event
    }
}

/// An event sink that keeps the last event in a [`Trail`] before handing it on.
pub(crate) struct Traced<'a> {
    /// Where the events go.
    pub(crate) sink: &'a dyn Events,
    /// Where the last one is kept.
    pub(crate) trail: &'a Trail,
}

impl Events for Traced<'_> {
    fn emit(&self, event: &Event) {
        self.trail.keep(event);
        self.sink.emit(event);
    }
}

/// Events as text for a person, on standard error.
pub(crate) struct Human<'a>(pub(crate) &'a Term);

impl Events for Human<'_> {
    fn emit(&self, event: &Event) {
        if let Some(line) = human(&event.kind) {
            self.0.err(&line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Buf;

    /// A writer that fails every write with `kind`.
    struct Failing(ErrorKind);

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::from(self.0))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_reader_that_left_is_no_error_and_a_full_disk_is_said_once_done() {
        let closed = Term::new(Box::new(Failing(ErrorKind::BrokenPipe)), Box::new(std::io::sink()));
        closed.out("a line");
        assert!(closed.finish().is_ok());
        let full = Term::new(Box::new(Failing(ErrorKind::StorageFull)), Box::new(std::io::sink()));
        full.out("a line");
        full.out("another");
        assert!(matches!(full.finish(), Err(Error::Output(e)) if e.kind() == ErrorKind::StorageFull));
        assert!(full.finish().is_ok(), "said once");
    }

    #[test]
    fn output_and_notes_go_to_their_own_streams() {
        let (out, err) = (Buf::default(), Buf::default());
        let term = Term::new(Box::new(out.clone()), Box::new(err.clone()));
        term.out("answer");
        term.err("note");
        assert_eq!(out.take(), "answer\n");
        assert_eq!(err.take(), "note\n");
    }
}
