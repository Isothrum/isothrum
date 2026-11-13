//! The last lines a tool printed on standard error: what leads its error when it fails.

use std::collections::VecDeque;

use crate::tool_output::ruby::{is_backtrace_frame, is_harmless_warning};

/// How many standard error lines are kept.
const KEPT: usize = 10;

/// A tool's last [`KEPT`] standard error lines, a Ruby backtrace's frames and known-harmless
/// warnings left out: what leads its error when it fails.
#[derive(Debug, Default)]
pub(crate) struct Tail(VecDeque<String>);

impl Tail {
    /// Keeps `line`, the oldest dropped past [`KEPT`]; a backtrace frame or a harmless warning
    /// isn't kept.
    pub(crate) fn push(&mut self, line: &str) {
        if is_backtrace_frame(line) || is_harmless_warning(line) {
            return;
        }
        if self.0.len() == KEPT {
            self.0.pop_front();
        }
        self.0.push_back(line.to_owned());
    }

    /// The lines kept, oldest first.
    pub(crate) fn into_lines(self) -> Vec<String> {
        self.0.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_keeps_the_last_lines_without_backtrace_frames() {
        let mut tail = Tail::default();
        for i in 0..=KEPT {
            tail.push(&format!("line {i}"));
        }
        tail.push("\tfrom /usr/lib/vagrant/action/builder.rb:180:in `call'");
        let lines = tail.into_lines();
        assert_eq!(lines.len(), KEPT);
        assert_eq!(lines.first().map(String::as_str), Some("line 1"), "the oldest dropped");
        assert_eq!(lines.last().map(String::as_str), Some("line 10"), "the frame not kept");
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
