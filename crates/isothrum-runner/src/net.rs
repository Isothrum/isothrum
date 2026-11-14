//! The host's ports, behind [`HostPorts`]: whether one is taken, a free one to pin, and whether an
//! address answers. [`LoopbackPorts`] binds real sockets; the `fake` feature's `FakePorts` answers
//! from a test's setup.

#[expect(dead_code, reason = "the Compose driver's pinned host ports (not ported from the run manifest's `compose.pin_ports`) reserve them")]
mod reservations;
pub(crate) use reservations::PortReservations;

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, UdpSocket};
use std::time::Duration;

use crate::project::platform::Protocol;

use crate::BoxFuture;
use crate::port::HostPort;
pub(crate) use sealed::HeldPort;

/// The host's ports. Sealed: [`RunnerConfig::new`](crate::RunnerConfig::new) gives the real
/// host's (sockets on the loopback), and the `fake` feature's `FakePorts` answers from a test's
/// setup; no other implementation is needed, and only the runner asks them anything.
pub trait HostPorts: Send + Sync + sealed::Ports {}

impl<T: sealed::Ports> HostPorts for T {}

/// The real host: sockets on the loopback. Nothing listens on all interfaces: binding 0.0.0.0
/// made Windows Firewall ask whether the embedder's app may accept connections from the network
/// ([`held_on_all`] only binds there, and not on Windows).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LoopbackPorts;

/// What keeps [`HostPorts`] to this crate's implementations, and its questions to the runner.
pub(crate) mod sealed {
    use std::time::Duration;

    use crate::project::platform::Protocol;

    use crate::BoxFuture;
    use crate::port::HostPort;

    /// A free port, held until dropped so it isn't handed out twice while others are picked.
    pub struct HeldPort {
        /// The port.
        pub port: HostPort,
        _hold: Box<dyn Send + Sync>,
    }

    impl std::fmt::Debug for HeldPort {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("HeldPort").field("port", &self.port).finish_non_exhaustive()
        }
    }

    impl HeldPort {
        /// `port`, held by `hold` (a socket, or nothing for a fake).
        pub(crate) fn new(port: HostPort, hold: impl Send + Sync + 'static) -> HeldPort {
            HeldPort { port, _hold: Box::new(hold) }
        }
    }

    /// The host's ports, as the runner asks them. Implemented by this crate's ports only.
    pub trait Ports: Send + Sync {
        /// Whether `port` is taken over `protocol` on the loopback.
        fn in_use(&self, port: HostPort, protocol: Protocol) -> bool;
        /// A free loopback port over `protocol`, held; none when none is left.
        ///
        /// # Errors
        ///
        /// The loopback couldn't be bound (no file descriptor left, a loopback that is down).
        fn pick_free(&self, protocol: Protocol) -> std::io::Result<Option<HeldPort>>;
        /// Whether `host:port` accepts a TCP connection within `within` and keeps it: a port
        /// forwarded by a proxy (Docker's, a hypervisor's NAT) accepts every connection and
        /// closes it at once when nothing listens behind, which is not a service ready.
        fn reachable<'a>(&'a self, host: &'a str, port: u16, within: Duration) -> BoxFuture<'a, bool>;
    }
}

impl sealed::Ports for LoopbackPorts {
    /// Taken when the IPv4 loopback can't be bound, or the IPv6 one is held (a browser opening
    /// `localhost` may reach it first), or a program holds it on all interfaces
    /// ([`held_on_all`]); an absent IPv6 loopback doesn't count.
    fn in_use(&self, port: HostPort, protocol: Protocol) -> bool {
        let port = port.get();
        let v4 = bind(Ipv4Addr::LOCALHOST.into(), port, protocol);
        let v6 = bind(Ipv6Addr::LOCALHOST.into(), port, protocol);
        v4.is_err() || v6.is_err_and(|e| held_elsewhere(&e)) || held_on_all(port, protocol)
    }

    /// A port the OS offers on the IPv4 loopback that [`in_use`](sealed::Ports::in_use) wouldn't call
    /// taken: one held on the IPv6 loopback is passed over ([`free_on_both`]).
    fn pick_free(&self, protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        free_on_both(protocol, || match protocol {
            Protocol::Udp => {
                let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
                Ok(HeldPort::new(HostPort::new(s.local_addr()?.port()), s))
            }
            Protocol::Tcp => {
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
                Ok(HeldPort::new(HostPort::new(l.local_addr()?.port()), l))
            }
        })
    }

    fn reachable<'a>(&'a self, host: &'a str, port: u16, within: Duration) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            match tokio::time::timeout(within, tokio::net::TcpStream::connect((host, port))).await {
                Ok(Ok(stream)) => kept_open(stream).await,
                _ => false,
            }
        })
    }
}

/// How long an accepted connection is watched for the peer closing it: a proxy with nothing
/// behind closes within milliseconds; a service that waits for the client's first bytes keeps it.
const HOLD: Duration = Duration::from_millis(500);

/// Whether the peer keeps `stream` open for [`HOLD`] (or speaks first): closed at once is a
/// proxy's accept with nothing listening behind.
async fn kept_open(stream: tokio::net::TcpStream) -> bool {
    let mut first = [0u8; 1];
    match tokio::time::timeout(HOLD, stream.peek(&mut first)).await {
        Ok(Ok(0) | Err(_)) => false,
        Ok(Ok(_)) | Err(_) => true,
    }
}

/// How many ports the OS may offer a pick before it gives up.
const PICKS: usize = 16;

/// The first port `next` holds on the IPv4 loopback that the IPv6 one doesn't hold either, as
/// [`LoopbackPorts`]'s `in_use` counts a port; each one passed over stays held meanwhile, so it
/// isn't offered again. None after [`PICKS`] offers.
fn free_on_both(protocol: Protocol, mut next: impl FnMut() -> std::io::Result<HeldPort>) -> std::io::Result<Option<HeldPort>> {
    let mut passed = Vec::new();
    for _ in 0..PICKS {
        let held = next()?;
        if bind(Ipv6Addr::LOCALHOST.into(), held.port.get(), protocol).is_err_and(|e| held_elsewhere(&e)) {
            passed.push(held);
        } else {
            return Ok(Some(held));
        }
    }
    Ok(None)
}

/// Binds (then frees) `ip:port` over `protocol`.
fn bind(ip: IpAddr, port: u16, protocol: Protocol) -> std::io::Result<()> {
    match protocol {
        Protocol::Udp => UdpSocket::bind((ip, port)).map(drop),
        Protocol::Tcp => TcpListener::bind((ip, port)).map(drop),
    }
}

/// Whether a program holds `port` over `protocol` on all interfaces (`0.0.0.0`, `::`), as an
/// engine publishing a port for the network does. macOS lets the loopback be bound with address
/// reuse (which std's listeners set) while another socket listens on all interfaces, so the
/// loopback alone read such a port free, and the start failed later with the engine's bind error.
/// Bound, never listened on, so no firewall asks about it; an absent IPv6 doesn't count. Not on
/// Windows, whose firewall asks about a socket on all interfaces, and where std sets no address
/// reuse for the loopback's bind to step past such a socket.
#[cfg(not(windows))]
fn held_on_all(port: u16, protocol: Protocol) -> bool {
    [IpAddr::from(Ipv4Addr::UNSPECIFIED), IpAddr::from(Ipv6Addr::UNSPECIFIED)]
        .into_iter()
        .any(|ip| bind_only(ip, port, protocol).is_err_and(|e| e.kind() == std::io::ErrorKind::AddrInUse))
}

/// Windows: see the other platforms' `held_on_all`.
#[cfg(windows)]
fn held_on_all(_port: u16, _protocol: Protocol) -> bool {
    false
}

/// Binds (then frees) `ip:port` over `protocol` without listening, with address reuse as std's
/// TCP listeners set it (a port only left in `TIME_WAIT` is free), `::` for IPv6 alone.
#[cfg(not(windows))]
fn bind_only(ip: IpAddr, port: u16, protocol: Protocol) -> std::io::Result<()> {
    use socket2::{Domain, Socket, Type};
    let address = std::net::SocketAddr::new(ip, port);
    let kind = match protocol {
        Protocol::Tcp => Type::STREAM,
        Protocol::Udp => Type::DGRAM,
    };
    let socket = Socket::new(Domain::for_address(address), kind, None)?;
    if matches!(protocol, Protocol::Tcp) {
        socket.set_reuse_address(true)?;
    }
    if ip.is_ipv6() {
        socket.set_only_v6(true)?;
    }
    socket.bind(&address.into())
}

/// Whether a failed bind means someone holds the port (or the OS reserved it, as Windows does
/// for WinNAT and Hyper-V), not that the address doesn't exist here.
fn held_elsewhere(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied)
}

/// Ports answered from a test's setup (feature `fake`): the free ones handed out in order. An
/// embedder's tests script those only; the runner's own also mark ports taken, addresses
/// answering and binds refused.
#[cfg(any(test, feature = "fake"))]
#[derive(Debug, Default)]
pub struct FakePorts {
    taken: std::sync::Mutex<std::collections::BTreeSet<(u16, Protocol)>>,
    free: std::sync::Mutex<std::collections::VecDeque<u16>>,
    answering: std::sync::Mutex<std::collections::BTreeSet<(String, u16)>>,
    /// What a pick fails with instead of handing out a port, if anything.
    unbindable: std::sync::Mutex<Option<std::io::ErrorKind>>,
}

#[cfg(any(test, feature = "fake"))]
impl FakePorts {
    /// No port taken; `free` handed out in order.
    pub fn new(free: impl IntoIterator<Item = u16>) -> FakePorts {
        FakePorts {
            free: std::sync::Mutex::new(free.into_iter().collect()),
            ..FakePorts::default()
        }
    }
}

#[cfg(any(test, feature = "fake"))]
impl sealed::Ports for FakePorts {
    fn in_use(&self, port: HostPort, protocol: Protocol) -> bool {
        self.taken
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(port.get(), protocol))
    }

    fn pick_free(&self, _protocol: Protocol) -> std::io::Result<Option<HeldPort>> {
        if let Some(kind) = *self.unbindable.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            return Err(kind.into());
        }
        Ok(self
            .free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .map(|p| HeldPort::new(HostPort::new(p), ())))
    }

    fn reachable<'a>(&'a self, host: &'a str, port: u16, within: Duration) -> BoxFuture<'a, bool> {
        let up = self
            .answering
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&(host.to_owned(), port));
        Box::pin(async move {
            if !up {
                tokio::time::sleep(within).await;
            }
            up
        })
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_port_that_accepts_and_closes_at_once_isnt_reachable_one_that_holds_is() {
        use tokio::io::AsyncWriteExt as _;
        let ports = super::LoopbackPorts;
        let within = Duration::from_secs(5);
        // Nothing listening.
        let free = ports.pick_free(Protocol::Tcp).unwrap().unwrap().port;
        assert!(!ports.reachable("127.0.0.1", free.get(), within).await);
        // A proxy's accept: closed at once.
        let closing = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closing_port = closing.local_addr().unwrap().port();
        drop(tokio::spawn(async move {
            let (s, _) = closing.accept().await.unwrap();
            drop(s);
        }));
        assert!(!ports.reachable("127.0.0.1", closing_port, within).await);
        // A service that greets.
        let greeting = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let greeting_port = greeting.local_addr().unwrap().port();
        drop(tokio::spawn(async move {
            let (mut s, _) = greeting.accept().await.unwrap();
            s.write_all(b"220 ready\r\n").await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        }));
        assert!(ports.reachable("127.0.0.1", greeting_port, within).await);
        // A service that waits for the client's first bytes.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_port = silent.local_addr().unwrap().port();
        drop(tokio::spawn(async move {
            let (_held, _) = silent.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(2)).await;
        }));
        assert!(ports.reachable("127.0.0.1", silent_port, within).await);
    }

    use super::sealed::Ports;
    use super::*;

    /// A port free over both TCP and UDP, below every OS's ephemeral range, so nothing else is
    /// handed it while the test runs.
    fn port_free_over_both() -> u16 {
        let start = 20_000 + u16::try_from(std::process::id() % 10_000).unwrap();
        (start..32_768)
            .chain(20_000..start)
            .find(|&p| !LoopbackPorts.in_use(HostPort::new(p), Protocol::Tcp) && !LoopbackPorts.in_use(HostPort::new(p), Protocol::Udp))
            .unwrap()
    }

    #[test]
    fn a_held_port_is_in_use_over_its_protocol_only() {
        let tcp_port = port_free_over_both();
        let _tcp = TcpListener::bind(("127.0.0.1", tcp_port)).unwrap();
        assert!(LoopbackPorts.in_use(HostPort::new(tcp_port), Protocol::Tcp));
        assert!(!LoopbackPorts.in_use(HostPort::new(tcp_port), Protocol::Udp));
        let udp_port = port_free_over_both();
        let _udp = UdpSocket::bind(("127.0.0.1", udp_port)).unwrap();
        assert!(
            LoopbackPorts.in_use(HostPort::new(udp_port), Protocol::Udp),
            "a UDP holder (dnsmasq on 53) is caught"
        );
        assert!(!LoopbackPorts.in_use(HostPort::new(udp_port), Protocol::Tcp));
    }

    /// Regression: only the loopback was bound, which macOS allows while another program listens
    /// on all interfaces, so such a port read free. (Linux refuses that bind already: there this
    /// holds either way.)
    #[test]
    fn a_port_held_on_all_interfaces_is_in_use() {
        let tcp_port = port_free_over_both();
        assert!(!held_on_all(tcp_port, Protocol::Tcp));
        let _tcp = TcpListener::bind((Ipv4Addr::UNSPECIFIED, tcp_port)).unwrap();
        assert!(LoopbackPorts.in_use(HostPort::new(tcp_port), Protocol::Tcp));
        #[cfg(not(windows))]
        assert!(held_on_all(tcp_port, Protocol::Tcp), "{tcp_port} held on 0.0.0.0");
        let udp_port = port_free_over_both();
        let _udp = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, udp_port)).unwrap();
        assert!(LoopbackPorts.in_use(HostPort::new(udp_port), Protocol::Udp));
        #[cfg(not(windows))]
        assert!(held_on_all(udp_port, Protocol::Udp), "{udp_port} held on 0.0.0.0");
        if let Ok(v6) = TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)) {
            let port = v6.local_addr().unwrap().port();
            assert!(LoopbackPorts.in_use(HostPort::new(port), Protocol::Tcp), "{port} held on ::");
        }
    }

    /// A port a closed connection left in `TIME_WAIT` stays free: the check on all interfaces
    /// reuses the address as a listener would.
    #[cfg(not(windows))]
    #[test]
    fn a_port_left_in_time_wait_is_free() {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let (served, _) = listener.accept().unwrap();
        drop(served); // The server closes first: its side waits in TIME_WAIT.
        drop(client);
        drop(listener);
        assert!(!held_on_all(port, Protocol::Tcp), "{port}");
    }

    #[test]
    fn only_a_held_or_reserved_address_counts_on_the_ipv6_loopback() {
        use std::io::{Error, ErrorKind};
        assert!(held_elsewhere(&Error::from(ErrorKind::AddrInUse)));
        assert!(held_elsewhere(&Error::from(ErrorKind::PermissionDenied)));
        assert!(!held_elsewhere(&Error::from(ErrorKind::AddrNotAvailable)), "no IPv6 loopback here");
    }

    #[test]
    fn picked_ports_are_held_until_dropped() {
        let a = LoopbackPorts.pick_free(Protocol::Tcp).unwrap().unwrap();
        let b = LoopbackPorts.pick_free(Protocol::Tcp).unwrap().unwrap();
        assert_ne!(a.port, b.port);
        assert!(LoopbackPorts.in_use(a.port, Protocol::Tcp));
    }

    /// Regression: a pick bound the IPv4 loopback only, so it could hand out a port held on the
    /// IPv6 one, which `in_use` calls taken. Such a port is passed over (kept held meanwhile).
    #[test]
    fn a_port_held_on_the_ipv6_loopback_is_passed_over() {
        let Ok(v6) = TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
            return; // No IPv6 loopback here: nothing can hold a port on it.
        };
        let held = v6.local_addr().unwrap().port();
        let Ok(v4) = TcpListener::bind((Ipv4Addr::LOCALHOST, held)) else {
            return; // The OS took the IPv4 port too: nothing to pass over.
        };
        let mut offers = vec![HeldPort::new(HostPort::new(held), v4)].into_iter();
        let picked = free_on_both(Protocol::Tcp, || {
            Ok(offers.next().unwrap_or_else(|| {
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
                HeldPort::new(HostPort::new(l.local_addr().unwrap().port()), l)
            }))
        })
        .unwrap()
        .unwrap();
        assert_ne!(picked.port, HostPort::new(held));
        assert!(LoopbackPorts.in_use(HostPort::new(held), Protocol::Tcp), "{held} held on ::1");
    }

    #[tokio::test]
    async fn a_listening_address_is_reachable() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(LoopbackPorts.reachable("127.0.0.1", port, Duration::from_secs(2)).await);
        drop(listener);
        assert!(!LoopbackPorts.reachable("127.0.0.1", port, Duration::from_secs(2)).await);
    }
}
