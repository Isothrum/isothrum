use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use super::esxi::{self, Ssh};
use super::{NoCertificate, Unreachable, certificate, reach};

const LIMIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn a_closed_port_is_unreachable() {
    // Nothing can listen on port 0, so the connection fails at once.
    let e = reach("127.0.0.1", 0, LIMIT).await.unwrap_err();
    assert!(matches!(e, Unreachable::Failed { port: 0, .. }), "{e:?}");
    assert!(e.to_string().starts_with("can't reach 127.0.0.1:0"), "{e}");
}

#[tokio::test]
async fn an_open_port_is_reached_and_esxi_must_answer_as_ssh() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        for answer in [&b""[..], b"SSH-2.0-OpenSSH_9.6\r\n", b"HTTP/1.1 400 Bad Request\r\n\r\n"] {
            let (mut s, _) = listener.accept().await.unwrap();
            s.write_all(answer).await.unwrap();
        }
    });
    assert!(reach("127.0.0.1", port, LIMIT).await.unwrap() < LIMIT);
    match esxi::ssh("127.0.0.1", port, LIMIT).await.unwrap() {
        Ssh::Answers { banner, .. } => assert_eq!(banner, "SSH-2.0-OpenSSH_9.6"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(esxi::ssh("127.0.0.1", port, LIMIT).await.unwrap(), Ssh::NotSsh { .. }));
}

#[test]
fn an_ipv6_host_goes_in_brackets() {
    assert_eq!(super::host_for_url("fd00::5"), "[fd00::5]");
    assert_eq!(super::host_for_url("pve.example"), "pve.example");
}

#[tokio::test]
async fn a_servers_certificate_is_read_without_a_request_and_no_server_says_why() {
    use isothrum_runner::fake_https::{self as https, Request};
    let server = https::Server::start(|_: &Request| (200, "{}".to_owned())).await.unwrap();
    let port: u16 = server.endpoint.rsplit(':').next().unwrap().parse().unwrap();
    assert_eq!(certificate("127.0.0.1", port, Duration::from_secs(5)).await, Ok(https::fingerprint()));
    assert!(server.requests().is_empty(), "nothing sent");
    let none = certificate("127.0.0.1", 1, Duration::from_secs(5)).await.unwrap_err();
    assert!(matches!(&none, NoCertificate::Unread { port: 1, .. }), "{none:?}");
}
