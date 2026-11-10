//! A request's output log, read as the broker writes it.

use std::io::Read as _;
use std::path::Path;

use crate::events::Stream;
use crate::process::OnLine;
use crate::process::lines::take_line;

/// A growing log, read a line at a time from where the last read stopped.
#[derive(Debug, Default)]
pub(super) struct LogReader {
    offset: u64,
    partial: Vec<u8>,
}

impl LogReader {
    /// Hands `on_line` every whole line `log` gained since the last read.
    pub(super) fn read(&mut self, log: &Path, on_line: OnLine<'_>) -> std::io::Result<()> {
        let mut f = match std::fs::File::open(log) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        let read = f.read_to_end(&mut bytes)?;
        self.offset += u64::try_from(read).unwrap_or(0);
        self.partial.extend(bytes);
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.partial.drain(..=end).collect();
            on_line(Stream::Stdout, &take_line(&mut line));
        }
        Ok(())
    }

    /// The last line, when the log didn't end with a line break.
    pub(super) fn finish(&mut self, on_line: OnLine<'_>) {
        if !self.partial.is_empty() {
            on_line(Stream::Stdout, &take_line(&mut self.partial));
        }
    }
}
