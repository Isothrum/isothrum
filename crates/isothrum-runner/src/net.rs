//! The host's ports, behind [`HostPorts`]: whether one is taken, a free one to pin, and whether an
//! address answers. [`LoopbackPorts`] binds real sockets; the `fake` feature's `FakePorts` answers
//! from a test's setup.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpListener, UdpSocket};
use std::time::Duration;

use isoloom_manifest::Protocol;

use crate::BoxFuture;

/// A free port, held until dropped so it isn't handed out twice while others are picked.
pub struct HeldPort {
    /// The port.
    pub port: u16,
    _hold: Box<dyn Send + Sync>,
}

impl std::fmt::Debug for HeldPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldPort").field("port", &self.port).finish_non_exhaustive()
    }
}

impl HeldPort {
    /// `port`, held by `hold` (a socket, or nothing for a fake).
    pub(crate) fn new(port: u16, hold: impl Send + Sync + 'static) -> HeldPort {
        HeldPort { port, _hold: Box::new(hold) }
    }
}

/// The host's ports. Sealed: [`RunnerConfig::new`](crate::RunnerConfig::new) gives the real
/// host's (sockets on the loopback), and the `fake` feature's `FakePorts` answers from a test's
/// setup; no other implementation is needed.
pub trait HostPorts: Send + Sync + sealed::Sealed {
    /// Whether `port` is taken over `protocol` on the loopback.
    fn in_use(&self, port: u16, protocol: Protocol) -> bool;
    /// A free loopback port over `protocol`, held.
    fn pick_free(&self, protocol: Protocol) -> Option<HeldPort>;
    /// Whether `host:port` accepts a TCP connection within `within`.
    fn reachable<'a>(&'a self, host: &'a str, port: u16, within: Duration) -> BoxFuture<'a, bool>;
}

/// The real host: sockets on the loopback. Never all interfaces: binding 0.0.0.0 made Windows
/// Firewall ask whether the embedder's app may accept connections from the network.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LoopbackPorts;

/// What keeps [`HostPorts`] to this crate's implementations.
mod sealed {
    /// Implemented by this crate's ports only.
    pub trait Sealed {}

    impl Sealed for super::LoopbackPorts {}

    #[cfg(any(test, feature = "fake"))]
    impl Sealed for super::FakePorts {}
}

impl HostPorts for LoopbackPorts {
    /// Taken when the IPv4 loopback can't be bound, or the IPv6 one is held (a browser opening
    /// `localhost` may reach it first); an absent IPv6 loopback doesn't count.
    fn in_use(&self, port: u16, protocol: Protocol) -> bool {
        let v4 = bind(Ipv4Addr::LOCALHOST.into(), port, protocol);
        let v6 = bind(Ipv6Addr::LOCALHOST.into(), port, protocol);
        v4.is_err() || v6.is_err_and(|e| held_elsewhere(&e))
    }

    fn pick_free(&self, protocol: Protocol) -> Option<HeldPort> {
        match protocol {
            Protocol::Udp => {
                let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
                Some(HeldPort::new(s.local_addr().ok()?.port(), s))
            }
            Protocol::Tcp => {
                let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
                Some(HeldPort::new(l.local_addr().ok()?.port(), l))
            }
        }
    }

    fn reachable<'a>(&'a self, host: &'a str, port: u16, within: Duration) -> BoxFuture<'a, bool> {
        Box::pin(async move { matches!(tokio::time::timeout(within, tokio::net::TcpStream::connect((host, port))).await, Ok(Ok(_))) })
    }
}

/// Binds (then frees) `ip:port` over `protocol`.
fn bind(ip: IpAddr, port: u16, protocol: Protocol) -> std::io::Result<()> {
    match protocol {
        Protocol::Udp => UdpSocket::bind((ip, port)).map(drop),
        Protocol::Tcp => TcpListener::bind((ip, port)).map(drop),
    }
}

/// Whether a failed bind means someone holds the port (or the OS reserved it, as Windows does
/// for WinNAT and Hyper-V), not that the address doesn't exist here.
fn held_elsewhere(e: &std::io::Error) -> bool {
    matches!(e.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied)
}

/// Ports answered from a test's setup (feature `fake`): taken ones, the free ones handed out in
/// order, and the addresses that answer.
#[cfg(any(test, feature = "fake"))]
#[derive(Debug, Default)]
pub struct FakePorts {
    taken: std::sync::Mutex<std::collections::BTreeSet<(u16, Protocol)>>,
    free: std::sync::Mutex<std::collections::VecDeque<u16>>,
    answering: std::sync::Mutex<std::collections::BTreeSet<(String, u16)>>,
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

    /// Marks `port` taken over `protocol`.
    #[cfg(test)]
    pub(crate) fn take(&self, port: u16, protocol: Protocol) {
        self.taken.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((port, protocol));
    }

    /// Makes `host:port` answer.
    #[cfg(test)]
    pub(crate) fn answer(&self, host: &str, port: u16) {
        self.answering
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((host.to_owned(), port));
    }
}

#[cfg(any(test, feature = "fake"))]
impl HostPorts for FakePorts {
    fn in_use(&self, port: u16, protocol: Protocol) -> bool {
        self.taken.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(&(port, protocol))
    }

    fn pick_free(&self, _protocol: Protocol) -> Option<HeldPort> {
        self.free
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .map(|p| HeldPort::new(p, ()))
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
    use super::*;

    /// A port free over both TCP and UDP, below every OS's ephemeral range, so nothing else is
    /// handed it while the test runs.
    fn port_free_over_both() -> u16 {
        let start = 20_000 + u16::try_from(std::process::id() % 10_000).unwrap();
        (start..32_768)
            .chain(20_000..start)
            .find(|&p| !LoopbackPorts.in_use(p, Protocol::Tcp) && !LoopbackPorts.in_use(p, Protocol::Udp))
            .unwrap()
    }

    #[test]
    fn a_held_port_is_in_use_over_its_protocol_only() {
        let tcp_port = port_free_over_both();
        let _tcp = TcpListener::bind(("127.0.0.1", tcp_port)).unwrap();
        assert!(LoopbackPorts.in_use(tcp_port, Protocol::Tcp));
        assert!(!LoopbackPorts.in_use(tcp_port, Protocol::Udp));
        let udp_port = port_free_over_both();
        let _udp = UdpSocket::bind(("127.0.0.1", udp_port)).unwrap();
        assert!(LoopbackPorts.in_use(udp_port, Protocol::Udp), "a UDP holder (dnsmasq on 53) is caught");
        assert!(!LoopbackPorts.in_use(udp_port, Protocol::Tcp));
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
        let a = LoopbackPorts.pick_free(Protocol::Tcp).unwrap();
        let b = LoopbackPorts.pick_free(Protocol::Tcp).unwrap();
        assert_ne!(a.port, b.port);
        assert!(LoopbackPorts.in_use(a.port, Protocol::Tcp));
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
