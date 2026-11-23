//! What Ruby tools print on standard error that never says why they failed, read: a backtrace's
//! frames (Vagrant prints its error, then dozens of them) and the warnings of fog, the library
//! vagrant-libvirt calls through. A tool's tail (`process::tail`) leaves them out.

/// Whether `line` is a frame of a Ruby backtrace (`\tfrom /…/builder.rb:180:in 'call'`), which
/// never counts among a failure's last lines: Vagrant prints its error, then dozens of frames.
pub(crate) fn is_backtrace_frame(line: &str) -> bool {
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
pub(crate) fn is_harmless_warning(line: &str) -> bool {
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
}
