//! An ESXi host, driven over SSH: whether its SSH port answers as SSH (the banner a server
//! writes first, `SSH-2.0-…`). The password is checked when it is first used, not here.

use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::{Unreachable, connect};

/// What an ESXi host's SSH port answered.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Ssh {
    /// It answered as SSH, after `latency` to connect; `banner` is its first line.
    Answers {
        /// How long the connection took.
        latency: Duration,
        /// The server's identification line (`SSH-2.0-…`).
        banner: String,
    },
    /// The port is open but didn't answer as SSH within the time given (another service, or SSH
    /// turned off behind a proxy).
    NotSsh {
        /// How long the connection took.
        latency: Duration,
    },
}

/// What `host:port` answers: SSH or not, each wait within `limit`.
///
/// # Errors
///
/// Nothing accepted the connection ([`Unreachable`]).
pub async fn ssh(host: &str, port: u16, limit: Duration) -> Result<Ssh, Unreachable> {
    let (mut stream, latency) = connect(host, port, limit).await?;
    let mut buf = [0u8; 255];
    let read = tokio::time::timeout(limit, stream.read(&mut buf)).await;
    Ok(match read {
        Ok(Ok(n)) => banner(buf.get(..n).unwrap_or_default()).map_or(Ssh::NotSsh { latency }, |banner| Ssh::Answers { latency, banner }),
        _ => Ssh::NotSsh { latency },
    })
}

/// The first line of `bytes` when it is an SSH server's identification.
fn banner(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    text.starts_with("SSH-").then(|| text.lines().next().unwrap_or_default().trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_ssh_identification_is_a_banner() {
        assert_eq!(banner(b"SSH-2.0-OpenSSH_9.6\r\n").as_deref(), Some("SSH-2.0-OpenSSH_9.6"));
        assert_eq!(banner(b"HTTP/1.1 400 Bad Request\r\n"), None);
        assert_eq!(banner(b""), None);
    }
}
