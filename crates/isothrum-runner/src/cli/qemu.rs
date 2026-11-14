//! The QEMU adapter: its human monitor, asked over the Unix socket vagrant-qemu gives it, for
//! where its VNC server listens.

#[cfg(unix)]
use std::net::SocketAddr;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

/// How long the monitor gets to answer.
#[cfg(unix)]
const LIMIT: Duration = Duration::from_secs(10);

/// Where the VNC server of the QEMU whose monitor is at `socket` listens (`info vnc`); none when
/// it has none (a VM started without a screen).
///
/// # Errors
///
/// The monitor didn't answer (no socket there, a closed one, past [`LIMIT`]).
#[cfg(unix)]
pub(crate) async fn vnc_server(socket: &Path) -> std::io::Result<Option<SocketAddr>> {
    let said = ask_monitor(socket, "info vnc").await?;
    Ok(crate::tool_output::qemu::vnc_server(&said))
}

/// What QEMU's human monitor at `socket` answers `command`: its text up to the next prompt.
#[cfg(unix)]
async fn ask_monitor(socket: &Path, command: &str) -> std::io::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const PROMPT: &str = "(qemu) ";
    let talk = async {
        let mut stream = tokio::net::UnixStream::connect(socket).await?;
        let mut said = Vec::new();
        let mut buf = [0u8; 4096];
        // The banner, up to the first prompt.
        while !String::from_utf8_lossy(&said).contains(PROMPT) {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err(std::io::Error::other("the monitor closed before its prompt"));
            }
            said.extend(buf.iter().take(n));
        }
        stream.write_all(format!("{command}\n").as_bytes()).await?;
        let mut answer = Vec::new();
        while !String::from_utf8_lossy(&answer).contains(PROMPT) {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            answer.extend(buf.iter().take(n));
        }
        Ok(String::from_utf8_lossy(&answer).into_owned())
    };
    tokio::time::timeout(LIMIT, talk)
        .await
        .unwrap_or_else(|_| Err(std::io::Error::other(format!("no answer in {} seconds", LIMIT.as_secs()))))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A fake QEMU monitor on a real Unix socket: its banner, the command echoed, its answer.
    #[tokio::test]
    async fn the_monitor_is_asked_after_its_banner() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("m.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let monitor = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(b"QEMU 10.1.0 monitor - type 'help' for more information\r\n(qemu) ").await.unwrap();
            let mut buf = [0u8; 64];
            let n = s.read(&mut buf).await.unwrap();
            assert_eq!(&buf[..n], b"info vnc\n");
            s.write_all(b"info vnc\r\ndefault:\r\n  Server: 127.0.0.1:5902 (ipv4)\r\n(qemu) ").await.unwrap();
        });
        assert_eq!(vnc_server(&socket).await.unwrap(), Some("127.0.0.1:5902".parse().unwrap()));
        monitor.await.unwrap();
    }
}
