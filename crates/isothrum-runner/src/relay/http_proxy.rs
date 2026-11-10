//! A loopback HTTP forward proxy (plain HTTP only), for Vagrant's WinRM to Windows guests: the
//! WinRM client (`HTTPClient`) follows `http_proxy` and sends absolute-URI requests. Each client
//! connection gets one connection to its target, which WinRM's Negotiate auth needs.
//!
//! Any local process can reach a loopback port, and through it the LAN that macOS keeps third
//! party tools off. So the proxy only forwards what it is for: WinRM's plain-HTTP port on the
//! ESXi host or on an address of the local network, where the host's guests are (`Targets`).
//! Anything else (the internet, this machine's own services, another port), and a request it
//! can't read, is refused and its connection closed: answered with a 502 when it is the
//! connection's first request; a later one gets no answer of its own, since the connection's
//! responses are already its origin's.

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use super::{Relay, pump, serve};

const BAD_GATEWAY: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";
/// A request head longer than this is refused.
const MAX_HEAD: usize = 64 * 1024;
/// A request or header line longer than this, in bytes, is refused (WinRM's are well under 1 KB).
const MAX_LINE: u64 = 8 * 1024;
/// WinRM over plain HTTP, the only traffic the proxy carries.
const WINRM_PORT: u16 = 5985;

/// Where the proxy may forward: WinRM on the ESXi host it was started for, or on an address of
/// the local network (the host's guests get theirs from it, so they aren't known in advance).
#[derive(Clone, Debug)]
pub(crate) struct Targets {
    esxi_host: String,
    port: u16,
}

impl Targets {
    pub(crate) fn new(esxi_host: &str) -> Targets {
        Targets {
            esxi_host: super::bare_host(esxi_host).to_ascii_lowercase(),
            port: WINRM_PORT,
        }
    }

    /// Only `host:port`, for a test's own origin server on loopback (the pipe tests are macOS's).
    #[cfg(all(test, target_os = "macos"))]
    fn only(host: &str, port: u16) -> Targets {
        Targets { port, ..Targets::new(host) }
    }

    /// Whether `target` (`host:port`, as [`split_target`] gives it) may be reached.
    pub(crate) fn allows(&self, target: &str) -> bool {
        let Some((host, port)) = target.rsplit_once(':') else { return false };
        if port.parse::<u16>() != Ok(self.port) {
            return false;
        }
        let host = super::bare_host(host).to_ascii_lowercase();
        host == self.esxi_host || host.parse::<std::net::IpAddr>().is_ok_and(on_local_network)
    }
}

/// A private-network address (RFC 1918, IPv4 link-local, IPv6 unique-local or link-local): not
/// loopback, not the internet.
fn on_local_network(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        std::net::IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// The proxy, on a free loopback port, forwarding to `targets` only.
pub(crate) async fn start_http_proxy(targets: Targets) -> std::io::Result<Relay> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let local = listener.local_addr()?.port();
    let task = serve(listener, move |inbound| {
        let targets = targets.clone();
        async move {
            // A client gone mid-request ends its connection, never the proxy.
            if let Err(e) = proxy_connection(inbound, &targets).await {
                tracing::debug!(error = %e, "a proxied connection ended early");
            }
        }
    });
    Ok(Relay { port: local, task })
}

/// The request head's target as (host:port, origin-form path), from an absolute `http://` URI.
pub(super) fn split_target(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("http://")?;
    // The authority ends at the path, or at a query or fragment following it directly
    // (`http://host:5985?x=1` is a valid absolute-URI): the query stays on the request.
    let (authority, path) = match rest.find(['/', '?', '#']).and_then(|i| rest.split_at_checked(i)) {
        Some((authority, path)) if path.starts_with('/') => (authority, path.to_owned()),
        Some((authority, query)) => (authority, format!("/{query}")),
        None => (rest, "/".to_owned()),
    };
    if authority.is_empty() {
        return None;
    }
    let authority = if authority.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) && !authority.ends_with(']') {
        authority.to_owned()
    } else {
        format!("{authority}:80")
    };
    // A port with no host before it is no place to connect to.
    if authority.rsplit_once(':').is_some_and(|(host, _)| host.is_empty()) {
        return None;
    }
    Some((authority, path))
}

/// Rewrites one request head for the origin server: origin-form target, no proxy headers.
/// Returns the target and the body length.
pub(super) fn rewrite_head(head: &str) -> Option<(String, String, u64)> {
    // Lines end at LF, a CR before it optional: as `read_head` reads them. Split on CRLF only, a
    // header after a bare LF rode along hidden inside the line before it.
    let mut lines = head.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let mut first = lines.next()?.splitn(3, ' ');
    let (method, uri, version) = (first.next()?, first.next()?, first.next()?);
    let (target, path) = split_target(uri)?;
    let mut out = format!("{method} {path} {version}\r\n");
    let mut len = 0;
    for line in lines.filter(|l| !l.is_empty()) {
        let lower = line.to_ascii_lowercase();
        if lower.starts_with("proxy-connection:") || lower.starts_with("proxy-authorization:") {
            continue;
        }
        // A chunked body has no length to relay by: refused (a 502), not read as the next head.
        if lower.starts_with("transfer-encoding:") {
            return None;
        }
        if let Some(v) = lower.strip_prefix("content-length:") {
            // Only spaces and tabs are optional whitespace in HTTP (`str::trim` also takes \v, \f...).
            len = v.trim_matches([' ', '\t']).parse().ok()?;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    Some((target, out, len))
}

/// What the client sent next.
#[derive(Debug, PartialEq)]
enum Head {
    /// A request head, up to its blank line.
    Request(String),
    /// A line or a head over its limit: refused, not read on without end.
    TooLong,
    /// The client closed the connection.
    Closed,
}

/// Reads one request head (up to its blank line), skipping stray CRLFs between requests. No
/// line is read past [`MAX_LINE`], no head past [`MAX_HEAD`].
async fn read_head<R: tokio::io::AsyncBufRead + Unpin>(reader: &mut R) -> std::io::Result<Head> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        // One byte past the limit, to tell a line over it.
        let n = (&mut *reader).take(MAX_LINE + 1).read_line(&mut line).await?;
        if n == 0 {
            return Ok(if head.is_empty() { Head::Closed } else { Head::TooLong });
        }
        if u64::try_from(line.len()).map_or(true, |len| len > MAX_LINE) || !line.ends_with('\n') {
            return Ok(Head::TooLong);
        }
        let blank = line == "\r\n" || line == "\n";
        if blank && head.is_empty() {
            continue; // stray CRLF between requests
        }
        head.push_str(&line);
        if head.len() > MAX_HEAD {
            return Ok(Head::TooLong);
        }
        // A head ends at a blank line, a CR before its LF optional (as `rewrite_head` splits it).
        if blank {
            return Ok(Head::Request(head));
        }
    }
}

/// The client's side of a proxied connection: its write half until its first request reaches
/// an origin, then that origin's connection, whose pump writes the responses back to it.
enum Peer {
    Waiting(OwnedWriteHalf),
    Connected(Upstream),
}

/// The one origin a client connection is proxied to.
struct Upstream {
    /// `host:port`.
    target: String,
    to_origin: super::ToLan,
    /// Streams the origin's responses back to the client, in order (it pairs them with its
    /// requests).
    pump: JoinHandle<()>,
}

impl Upstream {
    fn start(target: String, (mut from_origin, to_origin, nc): (super::FromLan, super::ToLan, tokio::process::Child), mut client: OwnedWriteHalf) -> Upstream {
        let pump = tokio::spawn(async move {
            // `nc` lives as long as the origin's answers flow.
            let _nc = nc;
            pump(&mut from_origin, &mut client).await;
        });
        Upstream { target, to_origin, pump }
    }
}

impl Peer {
    /// Whether a request to `target` may go on this connection: one origin per connection.
    fn may_send_to(&self, target: &str) -> bool {
        match self {
            Peer::Waiting(_) => true,
            Peer::Connected(upstream) => upstream.target == target,
        }
    }

    /// Refuses the request and closes: a 502 while the client's write half is still ours (its
    /// first request), nothing once it is the origin's responses'.
    async fn refuse(self) -> std::io::Result<()> {
        if let Peer::Waiting(mut client) = self {
            client.write_all(BAD_GATEWAY).await?;
        }
        Ok(())
    }

    /// The client is done: its end passed on to the origin, the last response let finish. nc's
    /// input is closed, not shut down: a pipe's shutdown leaves it open, so nc would never pass
    /// the end on, and the origin, nc and this connection's task would wait for each other.
    async fn finish(self) {
        if let Peer::Connected(Upstream { to_origin, pump, .. }) = self {
            drop(to_origin);
            if let Err(e) = pump.await {
                tracing::debug!(error = %e, "the responses' pump ended early");
            }
        }
    }

    /// The connection to `target`, connecting on the first request; the client is refused
    /// when the origin can't be reached.
    async fn connected(self, target: String) -> std::result::Result<Upstream, std::io::Result<()>> {
        match self {
            Peer::Connected(upstream) => Ok(upstream),
            Peer::Waiting(client) => match origin(&target) {
                Ok(origin) => Ok(Upstream::start(target, origin, client)),
                Err(e) => {
                    tracing::warn!(error = %e, target = %target, "the proxy couldn't reach the origin; the client is refused");
                    Err(Peer::Waiting(client).refuse().await)
                }
            },
        }
    }
}

/// The origin `target` (`host:port`, as [`split_target`] gives it), through `nc`.
fn origin(target: &str) -> std::io::Result<(super::FromLan, super::ToLan, tokio::process::Child)> {
    let (host, port) = target.rsplit_once(':').ok_or_else(|| std::io::Error::other("no port"))?;
    let port = port.parse().map_err(|_| std::io::Error::other("not a port"))?;
    super::connect(super::bare_host(host), port)
}

async fn proxy_connection(inbound: TcpStream, targets: &Targets) -> std::io::Result<()> {
    let (read_half, write_half) = inbound.into_split();
    let mut reader = BufReader::new(read_half);
    let mut peer = Peer::Waiting(write_half);
    loop {
        let head = match read_head(&mut reader).await? {
            Head::Request(head) => head,
            Head::TooLong => return peer.refuse().await,
            Head::Closed => {
                peer.finish().await;
                return Ok(());
            }
        };
        let parsed = rewrite_head(head.trim_end());
        let Some((target, rewritten, body_len)) = parsed.filter(|(t, ..)| targets.allows(t) && peer.may_send_to(t)) else {
            // Malformed, a target it isn't for, or a second origin on one connection: refuse
            // and close.
            return peer.refuse().await;
        };
        let mut upstream = match peer.connected(target).await {
            Ok(upstream) => upstream,
            Err(refused) => return refused,
        };
        upstream.to_origin.write_all(rewritten.as_bytes()).await?;
        let mut body = (&mut reader).take(body_len);
        tokio::io::copy(&mut body, &mut upstream.to_origin).await?;
        peer = Peer::Connected(upstream);
    }
}

#[cfg(test)]
mod proptests;
#[cfg(test)]
mod tests;
