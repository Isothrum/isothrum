//! The runner service as the binary runs it: a client starts it when none answers, one service
//! per home, `shutdown` and the idle time stop it.
#![expect(
    clippy::disallowed_methods,
    reason = "a test starts the binary and the programs it checks against: it is no adapter of the product"
)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use isothrum_client::{Client, ServicePaths};

const BIN: &str = env!("CARGO_BIN_EXE_isothrum");

/// Waits up to 10 seconds for `done`.
async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "{what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_starts_the_service_once_and_a_second_serve_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let paths = ServicePaths::new(home.path());
    let first = Client::connect(home.path(), Some(Path::new(BIN))).await.unwrap();
    let pid = first.service().pid;
    assert_ne!(pid, std::process::id(), "a process of its own");
    let second = Client::connect(home.path(), Some(Path::new(BIN))).await.unwrap();
    assert_eq!(second.service().pid, pid, "the running one is reused");

    let refused = Command::new(BIN).args(["serve", "--home"]).arg(home.path()).output().unwrap();
    assert_eq!(refused.status.code(), Some(1));
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains("already holds") && said.contains(&format!("pid {pid}")), "{said}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(paths.socket()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the socket is the owner's only");
    }
    assert!(std::fs::read_to_string(paths.log()).unwrap().contains("the runner service listens"));

    first.shutdown().await.unwrap();
    eventually("the service stops and says so", || paths.read_info().is_none()).await;
    assert!(Client::connect_running(home.path()).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_service_exits_on_its_own() {
    let home = tempfile::tempdir().unwrap();
    let mut service = Command::new(BIN)
        .args(["serve", "--idle-timeout", "1", "--home"])
        .arg(home.path())
        .spawn()
        .unwrap();
    eventually("it exits", || service.try_wait().unwrap().is_some()).await;
    assert!(service.wait().unwrap().success());
    assert!(ServicePaths::new(home.path()).read_info().is_none());
}

#[test]
fn an_operation_with_no_run_fails_before_any_service_is_started() {
    let home = tempfile::tempdir().unwrap();
    let run = tempfile::tempdir().unwrap();
    let out = Command::new(BIN).arg("down").arg(run.path()).arg("--home").arg(home.path()).output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(ServicePaths::new(home.path()).read_info().is_none(), "nothing to run: no service");
}
