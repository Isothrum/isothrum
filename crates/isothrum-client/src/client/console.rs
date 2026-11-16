//! A graphical console the service reached, held on a connection of its own: an SSH tunnel that
//! carries it stays open for as long as the connection does.

use super::{ClientError, Reply, Result};
use crate::proto::ReachedConsole;
use crate::wire::{Reader, Writer};

/// A graphical console ([`Client::graphical_console`](crate::Client::graphical_console)): where
/// it answers and how to sign in, and the connection that keeps its tunnel open. Dropping it
/// closes the connection, and the service ends the tunnel.
pub struct HeldConsole {
    /// The console.
    pub console: ReachedConsole,
    pub(super) reader: Reader,
    pub(super) _writer: Writer,
}

impl HeldConsole {
    /// Waits for the console's tunnel to end on its own (its `ssh` exited, the service stopped),
    /// and says how: the console no longer answers. A console reached through no tunnel ends only
    /// with the service.
    pub async fn closed(&mut self) -> String {
        loop {
            match self.reader.next::<Reply>().await {
                Ok(Some(Reply::ConsoleClosed { reason, .. })) => return reason,
                Ok(Some(_)) => {}
                Ok(None) => return "the runner service closed the connection".to_owned(),
                Err(e) => return format!("the runner service's connection broke: {e}"),
            }
        }
    }
}

impl std::fmt::Debug for HeldConsole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldConsole").field("console", &self.console).finish_non_exhaustive()
    }
}

/// The console the reply to a `graphical_console` gives.
pub(super) fn of_reply(reply: Option<Reply>) -> Result<ReachedConsole> {
    match reply {
        Some(Reply::Console { console, .. }) => Ok(*console),
        Some(Reply::Error { report, .. }) => Err(ClientError::Refused(Box::new(report))),
        Some(other) => Err(super::unexpected(&other)),
        None => Err(ClientError::closed()),
    }
}
