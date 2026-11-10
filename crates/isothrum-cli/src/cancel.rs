//! Cancellation: the signals that stop a command. Every tool runs in its own process group (a
//! job object on Windows), so a terminal's Ctrl-C or a CI runner cancelling its job reaches only
//! `isothrum`; it then drops the operation, which records the run as cut short and asks each tool
//! to stop (SIGINT to its process group; on Windows its tree is ended at once), waits for them
//! (30 seconds at most, a second signal ends them at once), and exits with the signal's code.

/// What asked the command to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// An interrupt: Ctrl-C (SIGINT).
    Interrupt,
    /// A request to end: SIGTERM (a CI runner cancelling its job, `kill`); on Windows the
    /// console's Ctrl-Break, its window closed, a log-off or a shutdown.
    Terminate,
}

impl Signal {
    /// The exit code a shell gives a process this signal ended: 128 + the signal's number.
    pub(crate) fn exit_code(self) -> u8 {
        match self {
            Signal::Interrupt => 130,
            Signal::Terminate => 143,
        }
    }

    /// Its name, as a person reads it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Signal::Interrupt => "SIGINT",
            Signal::Terminate => "SIGTERM",
        }
    }
}

/// The first signal that asks the command to stop.
///
/// # Errors
///
/// A handler the OS refused to install.
#[cfg(unix)]
pub(crate) async fn requested() -> std::io::Result<Signal> {
    use tokio::signal::unix::{SignalKind, signal};
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = interrupt.recv() => Ok(Signal::Interrupt),
        _ = terminate.recv() => Ok(Signal::Terminate),
    }
}

/// The first console event that asks the command to stop.
///
/// # Errors
///
/// A handler the OS refused to install.
#[cfg(windows)]
pub(crate) async fn requested() -> std::io::Result<Signal> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
    let (mut c, mut brk, mut close) = (ctrl_c()?, ctrl_break()?, ctrl_close()?);
    let (mut logoff, mut shutdown) = (ctrl_logoff()?, ctrl_shutdown()?);
    tokio::select! {
        _ = c.recv() => Ok(Signal::Interrupt),
        _ = brk.recv() => Ok(Signal::Terminate),
        _ = close.recv() => Ok(Signal::Terminate),
        _ = logoff.recv() => Ok(Signal::Terminate),
        _ = shutdown.recv() => Ok(Signal::Terminate),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_exits_as_a_shell_says_it() {
        assert_eq!((Signal::Interrupt.exit_code(), Signal::Terminate.exit_code()), (130, 143));
    }
}
