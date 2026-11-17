//! This computer's network as the system's own tools see it: a port the test holds, and the
//! public address the internet sees.

use std::net::{Ipv4Addr, TcpListener, UdpSocket};

use isothrum_machine::net::{self, PublicIpFailed, RESOLVERS};
use isothrum_runner::Protocol;

use crate::here;

/// A TCP port the test listens on is taken, and the system's tools name this process as its
/// holder.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn a_tcp_port_the_test_listens_on_is_taken_and_held_by_it() {
    let cx = here().await;
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
    let port = held.local_addr().expect("its address").port();
    assert!(!net::port_free(port, Protocol::Tcp), "port {port} is taken");
    let holder = net::port_holder(&cx, port, Protocol::Tcp).await;
    println!("port {port}: {holder:?}");
    assert_eq!(holder.map(|h| h.pid), Some(std::process::id()), "the system names this process");
}

/// A UDP port the test holds is taken.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn a_udp_port_the_test_holds_is_taken() {
    let held = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("a port");
    let port = held.local_addr().expect("its address").port();
    assert!(!net::port_free(port, Protocol::Udp), "port {port} is taken");
}

/// The services asked agree on a public address; skips when none answers (offline).
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_public_address_is_one_the_services_agree_on() {
    let cx = here().await;
    match net::public_ipv4(&cx, &RESOLVERS).await {
        Ok(ip) => assert!(net::is_public_ipv4(ip), "{ip} is public"),
        Err(PublicIpFailed::Unconfirmed(failed)) if failed.len() == RESOLVERS.len() => skip!("no service answered (offline?): {failed:?}"),
        Err(other) => panic!("{other}"),
    }
}
