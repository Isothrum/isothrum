//! How a client reads a service's `hello`, against a peer that speaks the protocol through this
//! crate's own public frames ([`isothrum_client::wire`], [`isothrum_client::proto`]): a service
//! of this protocol, of another, one that refuses or answers something else, closes, or isn't
//! there. The client against the real service is `isothrum-service`'s `tests/client_api.rs`: the
//! service depends on this crate, and the workspace keeps its dependencies one way.
#![cfg(unix)]

use std::path::Path;

use isothrum_client::proto::{PROTOCOL, Reply, Request};
use isothrum_client::wire::{LineReader, write_line};
use isothrum_client::{Client, ClientError, ServiceInfo, VERSION};

/// What a fake service on the home's socket answers a `hello` with: a reply, or nothing.
fn answering(home: &Path, reply: Option<Reply>) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use interprocess::local_socket::traits::tokio::Listener as _;
    use interprocess::local_socket::{GenericFilePath, ListenerOptions, ToFsName as _};
    let socket = home.join("s");
    let name = socket.as_path().to_fs_name::<GenericFilePath>()?;
    let listener = ListenerOptions::new().name(name).create_tokio()?;
    let info = ServiceInfo {
        pid: std::process::id(),
        started: "2026-11-15T00:00:00Z".to_owned(),
        version: VERSION.to_owned(),
        protocol: PROTOCOL,
        endpoint: socket.display().to_string(),
        epoch: 1,
        build: None,
    };
    std::fs::write(home.join("service.json"), serde_json::to_string(&info).map_err(std::io::Error::other)?)?;
    Ok(tokio::spawn(async move {
        let Ok(stream) = listener.accept().await else { return };
        let (recv, mut send) = tokio::io::split(stream);
        let mut lines = LineReader::new(recv);
        let _hello = lines.next::<Request>().await;
        if let Some(reply) = reply {
            write_line(&mut send, &reply).await.ok();
        }
    }))
}

#[tokio::test]
async fn a_hello_of_this_protocol_connects_and_any_other_answer_is_a_protocol_error() {
    let speaks = |protocol| Reply::Hello {
        id: 0,
        protocol,
        version: "9.9.9".into(),
    };
    for (reply, said) in [
        (speaks(PROTOCOL + 1), "speaks protocol"),
        (
            Reply::Error {
                id: 0,
                report: isothrum_runner::ErrorReport::new(isothrum_runner::ErrorCode::Usage, "go away"),
            },
            "go away",
        ),
        (Reply::Ok { id: 0 }, "expected the service's hello"),
    ] {
        let home = tempfile::tempdir().expect("a home");
        let serving = answering(home.path(), Some(reply)).expect("a fake service");
        let e = Client::connect_running(home.path()).await.expect_err("not this protocol's hello");
        assert!(matches!(&e, ClientError::Protocol(m) if m.contains(said)), "{e:?}");
        serving.await.expect("the fake");
    }
    let home = tempfile::tempdir().expect("a home");
    let serving = answering(home.path(), Some(speaks(PROTOCOL))).expect("a fake service");
    let client = Client::connect_running(home.path()).await.expect("this protocol");
    assert_eq!(client.service().version, "9.9.9", "the version its hello says");
    serving.await.expect("the fake");
}

#[tokio::test]
async fn a_service_that_closes_before_its_hello_or_isnt_there_is_unavailable() {
    let home = tempfile::tempdir().expect("a home");
    let serving = answering(home.path(), None).expect("a fake service");
    let closed = Client::connect_running(home.path()).await.expect_err("no hello");
    assert!(
        matches!(&closed, ClientError::Unavailable(m) if m == "the service closed the connection"),
        "{closed:?}"
    );
    serving.await.expect("the fake");
    // The fake ended: its socket is gone with it.
    let gone = Client::connect_running(home.path()).await.expect_err("nothing listens");
    assert!(matches!(&gone, ClientError::Unavailable(m) if m.contains("doesn't answer")), "{gone:?}");
    let none = tempfile::tempdir().expect("a home");
    let nothing = Client::connect_running(none.path()).await.expect_err("no service");
    assert!(
        matches!(&nothing, ClientError::Unavailable(m) if m.starts_with("no service runs for ")),
        "{nothing:?}"
    );
}

/// A binary started as the service that ends without answering (one that refuses `serve`) is
/// unavailable as soon as it ended, saying so and what it printed, not after the whole start wait.
#[tokio::test]
async fn a_service_that_ends_without_answering_is_unavailable_once_it_ended() {
    let home = tempfile::tempdir().expect("a home");
    // Installed once for every run, not written by this test: a new program's first run waits for
    // macOS's check of it, seconds on a loaded machine; and one this process wrote can be "Text
    // file busy" in a process another test forks. It notes when it ended (in seconds since the
    // epoch) in the home it is given: `serve --home <home> --detach`.
    let script = "#!/bin/sh\necho \"error: Unrecognized option: 'home'\" >&2\ndate +%s > \"$3/ended\"\nexit 2\n";
    let refusing = isothrum_test_support::installed_executable(Path::new(env!("CARGO_TARGET_TMPDIR")), "isothrum", script).expect("the stand-in");
    let refused = Client::connect(home.path(), Some(&refusing)).await.expect_err("nothing answers");
    let answered = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_secs();
    assert!(
        matches!(&refused, ClientError::Unavailable(m) if m.contains("ended without answering") && m.contains("Unrecognized option")),
        "{refused:?}"
    );
    // Once it ended, not after the 30 s a hung start is given: timed from its end, so however long
    // a loaded machine took to start it doesn't count.
    let ended: u64 = std::fs::read_to_string(home.path().join("ended"))
        .expect("it ran")
        .trim()
        .parse()
        .expect("a time");
    assert!(
        answered.saturating_sub(ended) < 15,
        "answered {}s after it ended",
        answered.saturating_sub(ended)
    );
}
