//! A request's output log, read as the broker writes it.

use std::io::Read as _;
use std::path::Path;
use std::time::Instant;

use crate::events::Stream;
use crate::process::OnLine;
use crate::process::lines::Lines;

/// A growing log, read a line at a time from where the last read stopped.
#[derive(Debug, Default)]
pub(super) struct LogReader {
    offset: u64,
    lines: Lines,
}

impl LogReader {
    /// Hands `on_line` every whole line `log` gained since the last read.
    pub(super) fn read(&mut self, log: &Path, on_line: OnLine<'_>) -> std::io::Result<()> {
        let Some(mut f) = crate::store::if_there(std::fs::File::open(log))? else {
            return Ok(());
        };
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        let read = f.read_to_end(&mut bytes)?;
        self.offset += u64::try_from(read).unwrap_or(0);
        let mut rest = &bytes[..];
        while !rest.is_empty() {
            let (used, line) = self.lines.feed(rest, Instant::now());
            rest = rest.get(used..).unwrap_or_default();
            if let Some(line) = line {
                on_line(Stream::Stdout, &line);
            }
        }
        Ok(())
    }

    /// The last line, when the log didn't end with a line break.
    pub(super) fn finish(&mut self, on_line: OnLine<'_>) {
        if let Some(line) = self.lines.finish() {
            on_line(Stream::Stdout, &line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_progress_meter_redrawn_in_the_log_is_read_before_its_line_ends() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("out-1.log");
        let (mut reader, mut lines) = (LogReader::default(), Vec::new());
        std::fs::write(&log, "copying\r\n\rDisk progress: 8%\rDisk progress: 9").unwrap();
        reader.read(&log, &mut |_, l| lines.push(l.to_owned())).unwrap();
        assert_eq!(lines, ["copying", "Disk progress: 8%"]);
        reader.finish(&mut |_, l| lines.push(l.to_owned()));
        assert_eq!(lines, ["copying", "Disk progress: 8%", "Disk progress: 9"]);
    }
}
