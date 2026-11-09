//! Relays for macOS: Local Network Privacy keeps the third-party tools a run starts off the local
//! network (Vagrant's Ruby, ovftool and Terraform's providers get "no route to host" to a LAN
//! server or a VMware guest), while Apple's own binaries and the embedding app are let through.
//! So a Vagrant run on an ESXi host talks to loopback relays this process runs (SSH and HTTPS to
//! the host), ovftool gets the HTTPS relay's port through a small wrapper ([`ovftool`]), Vagrant's
//! WinRM to Windows guests goes through a loopback HTTP proxy ([`http_proxy`]), and Vagrant's SSH
//! to the guests goes through Apple's `/usr/bin/nc` (`ISOLOOM_SSH_PROXY_COMMAND`).
//!
//! The relays live as long as the operation that started them ([`Relayed`], held by its context),
//! and only a run that needs them gets them: on macOS, the `vagrant` driver with the
//! `vmware_esxi` provider and an ESXi host that isn't this machine. Everything else runs as it is.

mod http_proxy;
mod ovftool;

use std::path::Path;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use self::http_proxy::{Targets, start_http_proxy};

/// The params naming the ESXi host and its SSH port, in the Vagrantfiles Isoloom writes.
const HOSTNAME: &str = "ESXI_HOSTNAME";
const HOSTPORT: &str = "ESXI_HOSTPORT";
/// What Vagrant's SSH to the guests goes through.
const PROXY_COMMAND: &str = "ISOLOOM_SSH_PROXY_COMMAND";

/// A loopback port forwarding to one LAN endpoint while it lives.
#[derive(Debug)]
pub(crate) struct Relay {
    /// The loopback port.
    pub(crate) port: u16,
    task: JoinHandle<()>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What a relayed operation needs kept alive, and the environment pointing its tools at it.
#[derive(Debug, Default)]
pub(crate) struct Relayed {
    /// Every process of the operation gets these last (they win).
    pub(crate) env: Vec<(String, String)>,
    _relays: Vec<Relay>,
}

/// `localhost`, or a loopback or unspecified address.
fn is_this_machine(host: &str) -> bool {
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare.eq_ignore_ascii_case("localhost") || bare.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// The reading half of a LAN connection.
pub(crate) type FromLan = Box<dyn AsyncRead + Send + Unpin>;
/// The writing half of a LAN connection.
pub(crate) type ToLan = Box<dyn AsyncWrite + Send + Unpin>;

/// A connection to `host:port` on the LAN, through Apple's `/usr/bin/nc`: the runner's own
/// process may be a third-party binary macOS keeps off the LAN too (a command line started from a
/// terminal; an app may be let through while its child tools are not), while Apple's binaries
/// never are. Not the [`crate::ProcessRunner`] port: this is a byte pipe for as long as the
/// connection lasts, not a tool run to its end; `nc` goes with the connection (`kill_on_drop`).
pub(crate) fn connect(host: &str, port: u16) -> std::io::Result<(FromLan, ToLan, tokio::process::Child)> {
    let mut child = tokio::process::Command::new(NC)
        .arg(host)
        .arg(port.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let missing = || std::io::Error::other("nc's pipes weren't opened");
    let from: FromLan = Box::new(child.stdout.take().ok_or_else(missing)?);
    let to: ToLan = Box::new(child.stdin.take().ok_or_else(missing)?);
    Ok((from, to, child))
}

/// Apple's netcat.
const NC: &str = "/usr/bin/nc";

/// Listens on 127.0.0.1 (a free port) and pipes every connection to `host:port`, the one endpoint
/// it was started for, whatever the client asks.
pub(crate) async fn start(host: String, port: u16) -> std::io::Result<Relay> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local = listener.local_addr()?.port();
    let task = serve(listener, move |inbound| {
        let host = host.clone();
        async move {
            if let Ok((mut from, to, _nc)) = connect(&host, port) {
                let (mut read, mut write) = inbound.into_split();
                tokio::join!(pump_and_close(&mut read, to), pump(&mut from, &mut write));
            }
        }
    });
    Ok(Relay { port: local, task })
}

/// Copies `from` into `to` to its end, then closes `to`: each way of a relayed connection, so a
/// half-close reaches the other side. A connection cut short ends its copy; it is traced, never an
/// error of the run.
async fn pump<R, W>(from: &mut R, to: &mut W)
where
    R: AsyncRead + Unpin + ?Sized,
    W: AsyncWrite + Unpin + ?Sized,
{
    if let Err(e) = tokio::io::copy(from, to).await {
        tracing::debug!(error = %e, "a relayed connection ended early");
    }
    if let Err(e) = to.shutdown().await {
        tracing::debug!(error = %e, "a relayed connection was already closed");
    }
}

/// [`pump`] into nc's input, then closes it: a pipe's shutdown leaves it open, and only its close
/// makes nc pass the client's end on to the host (an SSH session on ESXi ends only then).
async fn pump_and_close<R>(from: &mut R, mut to: ToLan)
where
    R: AsyncRead + Unpin + ?Sized,
{
    pump(from, &mut to).await;
    drop(to);
}

/// Accepts connections on `listener` until the task is aborted, each served by `serve` on a task
/// of its own. A refused handshake or fd exhaustion must not end a relay while a tool is still
/// pointed at it (nor spin hot): it waits a moment and keeps accepting.
fn serve<F, Fut>(listener: TcpListener, serve: F) -> JoinHandle<()>
where
    F: Fn(TcpStream) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            let Ok((inbound, _)) = listener.accept().await else {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            tokio::spawn(serve(inbound));
        }
    })
}

/// Whether a run needs the relays: on macOS, Vagrant on an ESXi host (`provider`), whose address
/// (`params`' `ESXI_HOSTNAME`) isn't this machine. The host, when it does.
pub(crate) fn esxi_host<'a>(macos: bool, vagrant: bool, provider: Option<&str>, params: &'a std::collections::BTreeMap<String, String>) -> Option<&'a str> {
    let host = params.get(HOSTNAME).map(String::as_str).filter(|h| !h.is_empty() && !is_this_machine(h))?;
    (macos && vagrant && provider == Some("vmware_esxi")).then_some(host)
}

/// The relays to ESXi host `host` (its SSH on `ssh_port`, its HTTPS, WinRM to its guests), and
/// the environment pointing Vagrant, its plugin and ovftool at them. `path` is the run's `PATH`;
/// the ovftool wrapper goes in a folder under `home`.
pub(crate) async fn prepare(host: &str, ssh_port: u16, home: &Path, path: Option<&str>) -> std::io::Result<Relayed> {
    let ssh = start(host.to_owned(), ssh_port).await?;
    let https = start(host.to_owned(), 443).await?;
    let mut env = vec![
        (HOSTNAME.to_owned(), "127.0.0.1".to_owned()),
        (HOSTPORT.to_owned(), ssh.port.to_string()),
        (PROXY_COMMAND.to_owned(), "/usr/bin/nc %h %p".to_owned()),
    ];
    // Windows guests: Vagrant's WinRM (plain HTTP to the guest) through the proxy. Only the
    // lowercase http_proxy: HTTPS (box downloads) doesn't read it.
    let winrm = start_http_proxy(Targets::new(host)).await?;
    env.push(("http_proxy".into(), format!("http://127.0.0.1:{}", winrm.port)));
    env.push(("no_proxy".into(), "localhost,127.0.0.1".into()));
    if let Some(real) = ovftool::real(path) {
        let dir = ovftool::wrapper_dir(home)?;
        env.push(("ISOTHRUM_REAL_OVFTOOL".into(), real));
        env.push(("ISOTHRUM_OVFTOOL_PORT".into(), https.port.to_string()));
        env.push(("PATH".into(), format!("{}:{}", dir.display(), path.unwrap_or_default())));
    }
    Ok(Relayed {
        env,
        _relays: vec![ssh, https, winrm],
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    // Through Apple's `/usr/bin/nc`: the relays are macOS's.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_relay_pipes_both_ways() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let echo_port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&b).await.unwrap();
        });
        let relay = start("127.0.0.1".into(), echo_port).await.unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", relay.port)).await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");
    }

    // Regression: a client that closed its side never reached the host (nc's stdin stayed open
    // until the relay ended), so every SSH session Vagrant opened on ESXi stayed open for the
    // whole operation, and ESXi refused new logins once it had about a hundred.
    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn a_client_that_closes_its_side_closes_it_at_the_host() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let host = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let host_port = host.local_addr().unwrap().port();
        let seen = tokio::spawn(async move {
            let (mut s, _) = host.accept().await.unwrap();
            let mut got = Vec::new();
            // Read to the end: only the client's close ends it.
            s.read_to_end(&mut got).await.unwrap();
            got
        });
        let relay = start("127.0.0.1".into(), host_port).await.unwrap();
        let mut c = TcpStream::connect(("127.0.0.1", relay.port)).await.unwrap();
        c.write_all(b"bye").await.unwrap();
        c.shutdown().await.unwrap();
        let got = tokio::time::timeout(std::time::Duration::from_secs(5), seen).await;
        assert_eq!(got.expect("the host saw the close").unwrap(), b"bye");
        drop(relay);
    }

    fn params(host: &str) -> BTreeMap<String, String> {
        BTreeMap::from([(HOSTNAME.to_owned(), host.to_owned())])
    }

    #[test]
    fn only_vagrant_on_an_esxi_host_from_macos_is_relayed() {
        let p = params("10.0.0.5");
        assert_eq!(esxi_host(true, true, Some("vmware_esxi"), &p), Some("10.0.0.5"));
        assert_eq!(esxi_host(false, true, Some("vmware_esxi"), &p), None, "not on Linux or Windows");
        assert_eq!(esxi_host(true, false, Some("vmware_esxi"), &p), None, "not Terraform");
        assert_eq!(esxi_host(true, true, Some("virtualbox"), &p), None);
        assert_eq!(esxi_host(true, true, Some("vmware_esxi"), &BTreeMap::new()), None);
    }

    #[test]
    fn a_host_on_this_machine_is_never_relayed() {
        for host in ["127.0.0.1", "127.0.0.2", "localhost", "LOCALHOST", "::1", "[::1]", "0.0.0.0", ""] {
            assert_eq!(esxi_host(true, true, Some("vmware_esxi"), &params(host)), None, "{host}");
        }
        assert!(!is_this_machine("10.0.0.5") && !is_this_machine("esxi.lan") && !is_this_machine("[fd00::5]"));
    }

    #[tokio::test]
    async fn a_relayed_run_points_vagrant_at_its_relays() {
        let home = tempfile::tempdir().unwrap();
        let r = prepare("10.0.0.5", 22, home.path(), Some("/usr/bin:/bin")).await.unwrap();
        let get = |k: &str| r.env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert_eq!(get(HOSTNAME).as_deref(), Some("127.0.0.1"));
        assert_ne!(get(HOSTPORT).as_deref(), Some("22"));
        assert_eq!(get(PROXY_COMMAND).as_deref(), Some("/usr/bin/nc %h %p"));
        assert!(get("http_proxy").is_some_and(|p| p.starts_with("http://127.0.0.1:")));
    }
}
