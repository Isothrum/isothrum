//! The servers a computer places machines on, checked before anything is placed there: whether
//! the server answers at all ([`reach`], a TCP connection and how long it took), and what it
//! answers as: a Proxmox VE API that accepts a sign-in and has the nodes, storages and bridges a
//! placement needs ([`proxmox`]), an ESXi host's SSH ([`esxi`]).
//!
//! Each finding is typed; how it is put to a user, and what they are told to fix, is the caller's.

use std::time::{Duration, Instant};

use tokio::net::TcpStream;

pub mod esxi;
pub mod proxmox;
pub mod ssh;

/// How long a server may take to accept a connection, or to answer once connected.
pub const REACH_TIMEOUT: Duration = Duration::from_secs(6);

/// A host as it goes in a URL: an IPv6 address in brackets.
#[must_use]
pub fn host_for_url(host: &str) -> String {
    if host.contains(':') { format!("[{host}]") } else { host.to_owned() }
}

/// Why a server couldn't be reached.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Unreachable {
    /// The connection was refused or failed (name not resolved, no route): why.
    #[error("can't reach {host}:{port}: {why}")]
    Failed {
        /// The host asked.
        host: String,
        /// The port asked.
        port: u16,
        /// Why, as the system said it.
        why: String,
    },
    /// Nothing answered within the time given.
    #[error("timed out reaching {host}:{port}")]
    TimedOut {
        /// The host asked.
        host: String,
        /// The port asked.
        port: u16,
    },
}

/// A connection to `host:port`, and how long it took to accept it, within `limit`.
///
/// # Errors
///
/// It wasn't accepted ([`Unreachable::Failed`]) or not within `limit` ([`Unreachable::TimedOut`]).
pub async fn connect(host: &str, port: u16, limit: Duration) -> Result<(TcpStream, Duration), Unreachable> {
    let started = Instant::now();
    match tokio::time::timeout(limit, TcpStream::connect((host, port))).await {
        Ok(Ok(stream)) => Ok((stream, started.elapsed())),
        Ok(Err(e)) => Err(Unreachable::Failed {
            host: host.to_owned(),
            port,
            why: e.to_string(),
        }),
        Err(_) => Err(Unreachable::TimedOut { host: host.to_owned(), port }),
    }
}

/// Whether `host:port` accepts a TCP connection within `limit`, and how long it took.
///
/// # Errors
///
/// As [`connect`].
pub async fn reach(host: &str, port: u16, limit: Duration) -> Result<Duration, Unreachable> {
    connect(host, port, limit).await.map(|(_, latency)| latency)
}

#[cfg(test)]
mod tests;
