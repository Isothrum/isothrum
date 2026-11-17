//! This computer's network, as a caller needs it before it acts: whether a host port is free
//! ([`port_free`]) and, when it isn't, which process holds it ([`port_holder`]), and its public
//! IPv4 address, as the internet sees it ([`public_ipv4`]): what a
//! cloud firewall opens to when it lets this computer alone in. Independent services, run by different
//! companies, are asked at once (`curl`, through the [`Context`]'s runner), and two must give the
//! same address: one that is down or blocked fails nothing, and one that answers wrong (a captive
//! portal's page, a compromised service) can't open a firewall to someone else.

use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::time::Duration;

use isothrum_runner::{HostPort, PortHolder, ProcessSpec, Protocol};
use tokio::task::JoinSet;

use crate::context::Context;
use crate::exec;

/// How long [`port_free`] waits for a listener on the loopback to answer: one there answers at once.
const LISTENER_ANSWER: Duration = Duration::from_millis(250);

/// How long each of the system's tools [`port_holder`] asks may take.
pub const HOLDER_LOOKUP: Duration = Duration::from_secs(4);

/// Whether nothing on this computer holds host `port` for `protocol` where a published port goes
/// (the loopback): a socket bound there and let go at once, nothing kept. A TCP port is also
/// taken when something answers on it: a listener on every address (`0.0.0.0`) doesn't stop a
/// loopback bind on every system (macOS lets one through), but it does answer. A free port can
/// still be taken a moment later: this is a check before acting, not a reservation.
#[must_use]
pub fn port_free(port: u16, protocol: Protocol) -> bool {
    let loopback = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    match protocol {
        Protocol::Tcp => TcpListener::bind(loopback).is_ok() && TcpStream::connect_timeout(&loopback, LISTENER_ANSWER).is_err(),
        Protocol::Udp => UdpSocket::bind(loopback).is_ok(),
    }
}

/// The process holding host `port` for `protocol` on this computer (its program's name and its
/// pid), when the system's own tools name one: `lsof`, then `ss` (macOS, Linux), PowerShell
/// (Windows), each run through the [`Context`]'s runner within [`HOLDER_LOOKUP`]. What a caller
/// says beside a port [`port_free`] found taken, so the user knows what to stop. Best-effort: none
/// when no tool answers or names a process (`ss` without root names none).
pub async fn port_holder(cx: &Context, port: u16, protocol: Protocol) -> Option<PortHolder> {
    isothrum_runner::port_holder(cx.runner.as_ref(), HostPort::new(port), protocol, HOLDER_LOOKUP).await
}

/// The services asked by default: each answers a plain-text address over HTTPS.
pub const RESOLVERS: [&str; 4] = [
    "https://checkip.amazonaws.com",
    "https://api.ipify.org",
    "https://ifconfig.me/ip",
    "https://icanhazip.com",
];

/// How long one service may take: one that hangs doesn't hold up the others' answer for long.
pub const RESOLVER_TIMEOUT: Duration = Duration::from_secs(8);

/// The most an answer may hold: an address is a few bytes, a page (a captive portal's) is no
/// answer.
const MAX_ANSWER: usize = 256;

/// Why no public address was agreed on.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PublicIpFailed {
    /// The services that answered gave different addresses (a VPN that changes address, two
    /// internet connections, a service lying): each service with its address.
    Disagree(Vec<(String, Ipv4Addr)>),
    /// Fewer than two gave one: each service that gave none, with why.
    Unconfirmed(Vec<(String, String)>),
}

impl std::fmt::Display for PublicIpFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PublicIpFailed::Disagree(seen) => {
                let said = seen.iter().map(|(url, ip)| format!("{url}: {ip}")).collect::<Vec<_>>().join(", ");
                write!(f, "the services that tell this computer's public address disagree ({said})")
            }
            PublicIpFailed::Unconfirmed(failed) => {
                let said = failed.iter().map(|(url, why)| format!("{url}: {why}")).collect::<Vec<_>>().join("; ");
                write!(f, "fewer than two services gave this computer's public address ({said})")
            }
        }
    }
}

impl std::error::Error for PublicIpFailed {}

/// This computer's public IPv4 address, as `resolvers` ([`RESOLVERS`], unless a caller names its
/// own) agree on it, all asked at once.
///
/// # Errors
///
/// [`PublicIpFailed`]: they disagree, or fewer than two gave one ([`agreed`]).
pub async fn public_ipv4(cx: &Context, resolvers: &[&str]) -> Result<Ipv4Addr, PublicIpFailed> {
    let mut asked = JoinSet::new();
    for (i, url) in resolvers.iter().enumerate() {
        let runner = std::sync::Arc::clone(&cx.runner);
        let spec = curl(url);
        asked.spawn(async move { (i, ask(runner.as_ref(), &spec).await) });
    }
    let mut answers: Vec<(usize, Result<Ipv4Addr, String>)> = Vec::with_capacity(resolvers.len());
    while let Some(joined) = asked.join_next().await {
        // A task panicking is a bug: said as that service's failure, not a hang.
        answers.push(joined.unwrap_or_else(|e| (usize::MAX, Err(e.to_string()))));
    }
    answers.sort_by_key(|(i, _)| *i);
    agreed(resolvers, &answers.into_iter().map(|(_, a)| a).collect::<Vec<_>>())
}

/// `curl` asking `url` for the address over IPv4 and HTTPS alone, following no redirect (a
/// captive portal's), failing on an HTTP error, within [`RESOLVER_TIMEOUT`].
fn curl(url: &str) -> ProcessSpec {
    let limit = RESOLVER_TIMEOUT.as_secs().to_string();
    let max = MAX_ANSWER.to_string();
    ProcessSpec::new(
        "curl",
        [
            "--ipv4",
            "--silent",
            "--show-error",
            "--fail",
            "--proto",
            "=https",
            "--max-time",
            &limit,
            "--max-filesize",
            &max,
            url,
        ],
    )
}

/// One service's answer: a public IPv4 address, or why not.
async fn ask(runner: &dyn isothrum_runner::ProcessRunner, spec: &ProcessSpec) -> Result<Ipv4Addr, String> {
    // curl ends itself at its `--max-time`; the probe's own limit only catches one that doesn't.
    let captured = exec::probe_spec(runner, RESOLVER_TIMEOUT + Duration::from_secs(2), spec)
        .await
        .map_err(|e| e.to_string())?;
    let body = captured.output().map_err(|failed| failed.to_string())?;
    if body.len() > MAX_ANSWER {
        return Err("an unexpected answer".to_owned());
    }
    let ip: Ipv4Addr = body.trim().parse().map_err(|_| "an unexpected answer".to_owned())?;
    if is_public_ipv4(ip) {
        Ok(ip)
    } else {
        Err(format!("{ip}, not a public address"))
    }
}

/// The address `answers` (one per service of `resolvers`, in their order) agree on: at least two
/// gave it, and none another one.
///
/// # Errors
///
/// [`PublicIpFailed::Disagree`] when two gave different ones, [`PublicIpFailed::Unconfirmed`]
/// when fewer than two gave one.
pub fn agreed(resolvers: &[&str], answers: &[Result<Ipv4Addr, String>]) -> Result<Ipv4Addr, PublicIpFailed> {
    let seen: Vec<(String, Ipv4Addr)> = resolvers
        .iter()
        .zip(answers)
        .filter_map(|(url, a)| a.as_ref().ok().map(|ip| ((*url).to_owned(), *ip)))
        .collect();
    match seen.as_slice() {
        [(_, first), ..] if seen.iter().any(|(_, ip)| ip != first) => Err(PublicIpFailed::Disagree(seen)),
        [(_, first), _, ..] => Ok(*first),
        _ => Err(PublicIpFailed::Unconfirmed(
            resolvers
                .iter()
                .zip(answers)
                .filter_map(|(url, a)| a.as_ref().err().map(|why| ((*url).to_owned(), why.clone())))
                .collect(),
        )),
    }
}

/// Whether `ip` is an address on the internet: not private, loopback, link-local, shared
/// (carrier-grade NAT), documentation, benchmarking, multicast, reserved or broadcast. Anything
/// else can't be where this computer's traffic comes from, as a cloud firewall sees it.
#[must_use]
pub fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let special = ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..128).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 240;
    !special
}

#[cfg(test)]
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;
    use crate::testing::answered_by;

    const ONE: &str = "https://one.example";
    const TWO: &str = "https://two.example";
    const THREE: &str = "https://three.example";

    /// Services each answering as `answer` says for its URL.
    fn services(answer: fn(&str) -> Reply) -> Context {
        answered_by(FakeProcesses::model(move |call| {
            assert_eq!(call.program, "curl");
            assert!(call.args.iter().any(|a| a == "=https") && call.args.iter().any(|a| a == "--ipv4"), "{call:?}");
            answer(call.args.last().map_or("", String::as_str))
        }))
    }

    #[tokio::test]
    async fn two_services_agreeing_give_the_address_though_one_is_down() {
        let cx = services(|url| match url {
            ONE => Reply::fail(28, "curl: (28) Operation timed out"),
            _ => Reply::ok("1.2.3.4\n"),
        });
        assert_eq!(public_ipv4(&cx, &[ONE, TWO, THREE]).await, Ok(Ipv4Addr::new(1, 2, 3, 4)));
    }

    #[tokio::test]
    async fn services_that_disagree_give_no_address() {
        let cx = services(|url| Reply::ok(if url == TWO { "5.6.7.8" } else { "1.2.3.4" }));
        let Err(PublicIpFailed::Disagree(seen)) = public_ipv4(&cx, &[ONE, TWO, THREE]).await else {
            panic!()
        };
        assert_eq!(seen[1], (TWO.to_owned(), Ipv4Addr::new(5, 6, 7, 8)));
    }

    #[tokio::test]
    async fn a_single_answer_a_page_or_a_private_address_is_not_enough() {
        let cx = services(|url| match url {
            ONE => Reply::ok("1.2.3.4"),
            TWO => Reply::ok("<html>Sign in to the network</html>"),
            _ => Reply::ok("192.168.1.10"),
        });
        let Err(PublicIpFailed::Unconfirmed(failed)) = public_ipv4(&cx, &[ONE, TWO, THREE]).await else {
            panic!()
        };
        assert_eq!(
            failed,
            [
                (TWO.to_owned(), "an unexpected answer".to_owned()),
                (THREE.to_owned(), "192.168.1.10, not a public address".to_owned()),
            ]
        );
        let missing = answered_by(FakeProcesses::script().expect("curl", Reply::missing()).build());
        assert!(matches!(public_ipv4(&missing, &[ONE, TWO]).await, Err(PublicIpFailed::Unconfirmed(f)) if f.len() == 2));
    }

    #[test]
    fn only_internet_addresses_are_public() {
        for special in [
            "10.0.0.1",
            "172.16.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.0.8",
            "198.18.0.1",
            "203.0.113.7",
            "224.0.0.1",
            "240.0.0.1",
            "0.1.2.3",
        ] {
            assert!(!is_public_ipv4(special.parse().unwrap()), "{special}");
        }
        assert!(is_public_ipv4(Ipv4Addr::new(1, 2, 3, 4)) && is_public_ipv4(Ipv4Addr::new(100, 128, 0, 1)));
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn a_taken_port_names_the_process_the_systems_tools_say_holds_it() {
        let cx = answered_by(
            FakeProcesses::script()
                .expect("lsof -nP -iTCP:8000 -sTCP:LISTEN -FpcL", Reply::ok("p4123\ncnginx\nLroot\n"))
                .build(),
        );
        assert_eq!(port_holder(&cx, 8000, Protocol::Tcp).await, Some(PortHolder::new(4123, "nginx")));
        // Neither tool names anyone (`ss` run without root shows no process): none.
        let nobody = answered_by(
            FakeProcesses::script()
                .expect("lsof -nP -iUDP:53", Reply::ok(""))
                .expect("ss -H -lunp", Reply::ok("UNCONN 0 0 0.0.0.0:53 0.0.0.0:*"))
                .build(),
        );
        assert_eq!(port_holder(&nobody, 53, Protocol::Udp).await, None);
    }

    #[test]
    fn a_port_something_holds_is_not_free_and_a_released_one_is() {
        // TCP: held on the loopback, then on every address (which answers on the loopback too).
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
        let port = held.local_addr().expect("its port").port();
        assert!(!port_free(port, Protocol::Tcp));
        drop(held);
        assert!(port_free(port, Protocol::Tcp), "released, it is free again");
        let everywhere = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a free port");
        let wide = everywhere.local_addr().expect("its port").port();
        assert!(!port_free(wide, Protocol::Tcp), "a listener on every address holds it");
        drop(everywhere);
        // UDP: its own sockets, apart from TCP's.
        let udp = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("a free port");
        let datagrams = udp.local_addr().expect("its port").port();
        assert!(!port_free(datagrams, Protocol::Udp));
        drop(udp);
        assert!(port_free(datagrams, Protocol::Udp));
    }
}
