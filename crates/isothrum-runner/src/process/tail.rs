//! The last lines a tool printed on standard error: what leads its error when it fails.

use std::collections::VecDeque;

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

/// Whether `line` is a frame of a Ruby backtrace (`\tfrom /…/builder.rb:180:in 'call'`), which
/// never counts among a failure's last lines: Vagrant prints its error, then dozens of frames.
fn is_backtrace_frame(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("from ")
        .is_some_and(|rest| rest.contains(":in `") || rest.contains(":in '"))
}

/// What a tool's library prints on standard error that never says why the tool failed: fog's
/// warnings, which vagrant-libvirt's every call prints (`[fog][WARNING] Unrecognized arguments:
/// libvirt_ip_command`, once per machine), and which otherwise stood in for the error Vagrant
/// printed on its standard output with `--machine-readable`.
const HARMLESS_WARNINGS: [&str; 1] = ["[fog][WARNING] "];

/// Whether `line` is a warning [`HARMLESS_WARNINGS`] lists, which never counts among a failure's
/// last lines.
fn is_harmless_warning(line: &str) -> bool {
    let line = line.trim_start();
    HARMLESS_WARNINGS.iter().any(|w| line.starts_with(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ruby_backtraces_frames_arent_a_failures_last_lines() {
        assert!(is_backtrace_frame("\tfrom /usr/lib/vagrant/action/builder.rb:180:in `call'"));
        assert!(is_backtrace_frame("    from <internal:kernel>:187:in 'loop'"));
        assert!(!is_backtrace_frame("wait_for.rb:9:in `block in wait_for': timeout (Fog::Errors::TimeoutError)"));
        assert!(!is_backtrace_frame("from here on, the VM is gone"));
    }

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
