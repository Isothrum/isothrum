//! The first and last lines a tool printed on standard error: what leads its error when it fails.
//! The first say the cause as often as the last (Vagrant's own message, before a long one from a
//! plugin), so a long error keeps both, the lines between left out.

use std::collections::VecDeque;

use crate::tool_output::ruby::{is_backtrace_frame, is_harmless_warning};

/// How many of the last standard error lines are kept.
const KEPT: usize = 10;

/// How many of the first standard error lines are kept.
const HEAD: usize = 5;

/// A tool's first [`HEAD`] and last [`KEPT`] standard error lines, a Ruby backtrace's frames and
/// known-harmless warnings left out: what leads its error when it fails.
#[derive(Debug, Default)]
pub(crate) struct Tail {
    head: Vec<String>,
    last: VecDeque<String>,
    dropped: usize,
}

impl Tail {
    /// Keeps `line`: among the first [`HEAD`], else among the last [`KEPT`], the oldest of those
    /// dropped. A backtrace frame or a harmless warning isn't kept.
    pub(crate) fn push(&mut self, line: &str) {
        if is_backtrace_frame(line) || is_harmless_warning(line) {
            return;
        }
        if self.head.len() < HEAD {
            self.head.push(line.to_owned());
            return;
        }
        if self.last.len() == KEPT {
            self.last.pop_front();
            self.dropped += 1;
        }
        self.last.push_back(line.to_owned());
    }

    /// The lines kept, oldest first, a line saying how many were left out between the first and
    /// the last.
    pub(crate) fn into_lines(self) -> Vec<String> {
        let gap = (self.dropped > 0).then(|| format!("[{} lines left out]", self.dropped));
        self.head.into_iter().chain(gap).chain(self.last).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_first_and_last_lines_without_backtrace_frames() {
        let mut tail = Tail::default();
        for i in 0..HEAD + KEPT + 2 {
            tail.push(&format!("line {i}"));
        }
        tail.push("\tfrom /usr/lib/vagrant/action/builder.rb:180:in `call'");
        let lines = tail.into_lines();
        assert_eq!(lines.len(), HEAD + 1 + KEPT);
        assert_eq!(lines[0], "line 0", "the first kept");
        assert_eq!(lines[HEAD], "[2 lines left out]");
        assert_eq!(lines[HEAD + 1], "line 7", "the oldest of the rest dropped");
        assert_eq!(lines.last().map(String::as_str), Some("line 16"), "the frame not kept");
    }

    #[test]
    fn a_short_error_is_kept_whole() {
        let mut tail = Tail::default();
        for i in 0..HEAD + KEPT {
            tail.push(&format!("line {i}"));
        }
        let lines = tail.into_lines();
        assert_eq!(lines.len(), HEAD + KEPT);
        assert!(lines.iter().all(|l| !l.contains("left out")));
    }

    /// Regression: on libvirt, a failed `vagrant status --machine-readable` said only fog's
    /// warnings, one per machine, which stood in for Vagrant's error.
    #[test]
    fn fogs_warnings_arent_a_failures_last_lines() {
        let mut tail = Tail::default();
        tail.push("[fog][WARNING] Unrecognized arguments: libvirt_ip_command");
        tail.push("The SSH command responded with a non-zero exit status.");
        tail.push("  [fog][WARNING] Unrecognized arguments: libvirt_ip_command");
        assert_eq!(tail.into_lines(), ["The SSH command responded with a non-zero exit status."]);
        assert!(!is_harmless_warning("[fog][ERROR] connection refused"));
    }
}
