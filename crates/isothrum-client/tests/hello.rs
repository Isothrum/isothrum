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
                code: isothrum_runner::ErrorCode::Usage,
                message: "go away".into(),
                detail: None,
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
