//! The WinRM proxy: what it forwards, what it refuses, and how it reads a request.

use super::*;

#[test]
fn content_length_takes_only_spaces_and_tabs_around_it() {
    assert_eq!(rewrite_head("POST http://h/x HTTP/1.1\r\nContent-Length: \t12 ").unwrap().2, 12);
    for v in ["\u{b}0", "\u{c}5", "1 2"] {
        assert!(rewrite_head(&format!("POST http://h/x HTTP/1.1\r\nContent-Length: {v}")).is_none(), "{v:?}");
    }
}

#[test]
fn proxy_heads_become_origin_form() {
    let (t, head, len) =
        rewrite_head("POST http://10.0.0.7:5985/wsman HTTP/1.1\r\nHost: 10.0.0.7:5985\r\nProxy-Connection: Keep-Alive\r\nContent-Length: 12").unwrap();
    assert_eq!(t, "10.0.0.7:5985");
    assert_eq!(head, "POST /wsman HTTP/1.1\r\nHost: 10.0.0.7:5985\r\nContent-Length: 12\r\n\r\n");
    assert_eq!(len, 12);
    assert_eq!(split_target("http://host").unwrap(), ("host:80".into(), "/".into()));
    assert!(rewrite_head("GET /relative HTTP/1.1").is_none());
}

#[test]
fn content_length_takes_the_last_header_and_refuses_signs_or_blanks_and_proxy_headers_go_whatever_their_case() {
    let head = |extra: &str| format!("POST http://h/x HTTP/1.1\r\nHost: h\r\n{extra}");
    assert_eq!(rewrite_head(&head("Content-Length: 5\r\nContent-Length: 7")).unwrap().2, 7);
    for bad in ["Content-Length: -1", "Content-Length:", "Content-Length: 1e3", "Content-Length: 0x10"] {
        assert!(rewrite_head(&head(bad)).is_none(), "{bad:?}");
    }
    let (_, out, len) = rewrite_head(&head("PROXY-AUTHORIZATION: Basic x\r\nproxy-connection: close\r\nX-Odd no colon")).unwrap();
    assert_eq!(len, 0);
    assert_eq!(out, "POST /x HTTP/1.1\r\nHost: h\r\nX-Odd no colon\r\n\r\n");
    // A request line with a missing part is malformed.
    assert!(rewrite_head("GET http://h/x").is_none());
    assert!(rewrite_head("").is_none());
}

#[test]
fn a_target_without_a_path_keeps_its_query_on_the_request() {
    // `http://host:5985?x=1` is a valid absolute-URI (RFC 3986 lets the query follow the
    // authority directly). It connects to `host:5985?x=1:80` (a DNS failure, so a 502) and
    // would send `/`. Fix: end the authority at the first of `/`, `?` or `#`, and keep the
    // query on the origin-form path (`/?x=1`).
    assert_eq!(
        split_target("http://10.0.0.7:5985?x=1").unwrap(),
        ("10.0.0.7:5985".to_owned(), "/?x=1".to_owned())
    );
}

#[test]
fn a_target_with_a_port_and_no_host_is_refused() {
    // Found by the property test: `http://:00/` read as host "" on port 0.
    assert_eq!(split_target("http://:00/"), None);
    assert_eq!(split_target("http://:5985"), None);
}

#[test]
fn split_target_defaults_the_port_and_keeps_ipv6() {
    assert_eq!(split_target("http://[fd00::7]/wsman").unwrap(), ("[fd00::7]:80".into(), "/wsman".into()));
    assert_eq!(split_target("http://[fd00::7]:5985/x").unwrap(), ("[fd00::7]:5985".into(), "/x".into()));
    assert!(split_target("https://host/").is_none());
    assert!(split_target("http:///path").is_none());
}

/// The proxy of an ESXi host at 10.0.0.5.
fn esxi() -> Targets {
    Targets::new("10.0.0.5")
}

/// What the proxy answers `request` with, until it closes.
async fn answer(targets: Targets, request: &[u8]) -> Vec<u8> {
    let proxy = start_http_proxy(targets).await.unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).await.unwrap();
    // The proxy may refuse and close before it has read all of it: what it answered counts.
    c.write_all(request).await.ok();
    let mut got = Vec::new();
    c.read_to_end(&mut got).await.ok();
    got
}

#[tokio::test]
async fn malformed_requests_get_a_502() {
    assert_eq!(answer(esxi(), b"GET /relative HTTP/1.1\r\nHost: x\r\n\r\n").await, BAD_GATEWAY);
}

#[test]
fn only_winrm_on_the_esxi_host_or_the_local_network_is_a_target() {
    let t = esxi();
    for ok in [
        "10.0.0.5:5985",
        "10.0.0.77:5985",
        "192.168.1.40:5985",
        "172.20.3.4:5985",
        "169.254.10.2:5985",
        "[fd00::7]:5985",
        "[fe80::1]:5985",
    ] {
        assert!(t.allows(ok), "{ok}");
    }
    for refused in [
        "10.0.0.5:22",
        "10.0.0.77:80",
        "127.0.0.1:5985",
        "localhost:5985",
        "[::1]:5985",
        "93.184.216.34:5985",
        "[2001:4860::8888]:5985",
        "evil.example.com:5985",
        "0.0.0.0:5985",
        "10.0.0.77",
        "10.0.0.77:x",
    ] {
        assert!(!t.allows(refused), "{refused}");
    }
    // A host by name: that name only, any case.
    let named = Targets::new("ESXi.lan");
    assert!(named.allows("esxi.lan:5985") && !named.allows("other.lan:5985") && !named.allows("esxi.lan:443"));
    assert!(Targets::new("[fd00::5]").allows("[fd00::5]:5985"));
}

#[tokio::test]
async fn a_target_it_is_not_for_gets_a_502_without_a_connection() {
    // A listener of this machine's: the proxy must not reach it for another process.
    let local = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = local.local_addr().unwrap().port();
    let req = format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\nHost: x\r\n\r\n");
    assert_eq!(answer(esxi(), req.as_bytes()).await, BAD_GATEWAY);
    let probe = tokio::time::timeout(std::time::Duration::from_millis(100), local.accept()).await;
    assert!(probe.is_err(), "the proxy connected to a target it isn't for");
    assert_eq!(answer(esxi(), b"GET http://93.184.216.34:5985/wsman HTTP/1.1\r\n\r\n").await, BAD_GATEWAY);
}

#[tokio::test]
async fn an_endless_line_or_head_gets_a_502() {
    let long_line = format!("GET http://10.0.0.5:5985/{} HTTP/1.1\r\n\r\n", "a".repeat(usize::try_from(MAX_LINE).unwrap()));
    assert_eq!(answer(esxi(), long_line.as_bytes()).await, BAD_GATEWAY);
    let many = format!(
        "GET http://10.0.0.5:5985/ HTTP/1.1\r\n{}\r\n",
        "X-Pad: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n".repeat(MAX_HEAD / 50 + 1)
    );
    assert_eq!(answer(esxi(), many.as_bytes()).await, BAD_GATEWAY);
}

#[tokio::test]
async fn heads_are_read_line_by_line_within_their_limits() {
    let mut r = BufReader::new(&b"\r\nGET http://h/ HTTP/1.1\r\nHost: h\r\n\r\n"[..]);
    assert_eq!(
        read_head(&mut r).await.unwrap(),
        Head::Request("GET http://h/ HTTP/1.1\r\nHost: h\r\n\r\n".into())
    );
    assert_eq!(read_head(&mut r).await.unwrap(), Head::Closed);
    // Cut off mid-head, or a line with no end within the limit.
    assert_eq!(
        read_head(&mut BufReader::new(&b"GET http://h/ HTTP/1.1\r\nHo"[..])).await.unwrap(),
        Head::TooLong
    );
    let endless = vec![b'a'; usize::try_from(MAX_LINE).unwrap() + 10];
    assert_eq!(read_head(&mut BufReader::new(&endless[..])).await.unwrap(), Head::TooLong);
}

// Through Apple's `/usr/bin/nc`: the relays are macOS's.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn proxy_keeps_one_origin_connection_for_several_requests() {
    let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = origin.local_addr().unwrap().port();
    // Answers each request on the same connection with the body it got (echo).
    tokio::spawn(async move {
        let (s, _) = origin.accept().await.unwrap();
        let mut r = BufReader::new(s);
        for _ in 0..2 {
            let mut len = 0;
            loop {
                let mut l = String::new();
                r.read_line(&mut l).await.unwrap();
                if let Some(v) = l.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                if l == "\r\n" {
                    break;
                }
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).await.unwrap();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
            r.get_mut().write_all(reply.as_bytes()).await.unwrap();
            r.get_mut().write_all(&body).await.unwrap();
        }
    });
    let proxy = start_http_proxy(Targets::only("127.0.0.1", port)).await.unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).await.unwrap();
    for msg in ["first", "second!"] {
        let req = format!(
            "POST http://127.0.0.1:{port}/wsman HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{msg}",
            msg.len()
        );
        c.write_all(req.as_bytes()).await.unwrap();
        let expect = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{msg}", msg.len());
        let mut got = vec![0; expect.len()];
        c.read_exact(&mut got).await.unwrap();
        assert_eq!(String::from_utf8(got).unwrap(), expect);
    }
}

// Regression: a client that closed its side never reached the origin (nc's input was shut down,
// which leaves a pipe open), so the origin's connection, nc and the proxy's task for it stayed
// until the operation ended.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_client_that_closes_its_side_closes_it_at_the_origin() {
    let origin = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = origin.local_addr().unwrap().port();
    let seen = tokio::spawn(async move {
        let (mut s, _) = origin.accept().await.unwrap();
        let mut got = Vec::new();
        // Read to the end: only the client's close ends it.
        s.read_to_end(&mut got).await.unwrap();
        got
    });
    let proxy = start_http_proxy(Targets::only("127.0.0.1", port)).await.unwrap();
    let mut c = TcpStream::connect(("127.0.0.1", proxy.port)).await.unwrap();
    let request = format!("GET http://127.0.0.1:{port}/wsman HTTP/1.1\r\nHost: x\r\n\r\n");
    c.write_all(request.as_bytes()).await.unwrap();
    c.shutdown().await.unwrap();
    let got = tokio::time::timeout(std::time::Duration::from_secs(5), seen).await;
    assert_eq!(got.expect("the origin saw the close").unwrap(), b"GET /wsman HTTP/1.1\r\nHost: x\r\n\r\n");
}
