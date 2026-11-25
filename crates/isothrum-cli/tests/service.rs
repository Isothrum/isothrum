//! The runner service as the binary runs it: a client starts it when none answers, one service
//! per home, `shutdown` and the idle time stop it.
#![expect(
    clippy::disallowed_methods,
    reason = "a test starts the binary and the programs it checks against: it is no adapter of the product"
)]

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use isothrum_client::{Client, ClientError, ServicePaths};
use isothrum_test_support::{ChildGuard, PidGuard, TempHome, alive, wait_until};

const BIN: &str = env!("CARGO_BIN_EXE_isothrum");

/// How long a test waits for the service to do what it does on its own: long, as it only bounds
/// a failure.
const PATIENCE: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_starts_the_service_once_and_a_second_serve_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let paths = ServicePaths::new(home.path());
    let first = Client::connect(home.path(), Some(Path::new(BIN))).await.unwrap();
    let pid = first.service().pid;
    // The service is detached: should the test fail, it goes with the test, not at its idle time.
    let _service = PidGuard::new(pid);
    assert_ne!(pid, std::process::id(), "a process of its own");
    let second = Client::connect(home.path(), Some(Path::new(BIN))).await.unwrap();
    assert_eq!(second.service().pid, pid, "the running one is reused");

    let env = TempHome::new();
    let refused = Command::new(BIN).envs(env.envs()).args(["serve", "--home"]).arg(home.path()).output().unwrap();
    assert_eq!(refused.status.code(), Some(1));
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains(&format!("pid {pid}")), "the refusal names the service holding the home: {said}");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(paths.socket()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the socket is the owner's only");
    }
    assert!(std::fs::read_to_string(paths.log()).unwrap().contains("the runner service listens"));

    first.shutdown().await.unwrap();
    assert!(wait_until(PATIENCE, || paths.read_info().is_none()), "the service stops and says so");
    // Gone, not just withdrawn: on Windows the home can't be removed while its store is open.
    assert!(wait_until(PATIENCE, || !alive(pid)), "the service's process ends");
    let gone = Client::connect_running(home.path()).await.err();
    assert!(matches!(gone, Some(ClientError::Unavailable(_))), "{gone:?}");
}

#[test]
fn an_idle_service_exits_0_on_its_own_and_withdraws_its_info() {
    let (home, env) = (tempfile::tempdir().unwrap(), TempHome::new());
    let mut serve = Command::new(BIN);
    serve.envs(env.envs()).args(["serve", "--idle-timeout", "1", "--home"]).arg(home.path());
    let mut service = ChildGuard::spawn(&mut serve).unwrap();
    let mut exited = None;
    assert!(
        wait_until(PATIENCE, || {
            exited = service.try_wait().unwrap();
            exited.is_some()
        }),
        "it exits"
    );
    assert_eq!(exited.and_then(|s| s.code()), Some(0));
    assert!(ServicePaths::new(home.path()).read_info().is_none());
}

#[test]
fn an_operation_with_no_run_fails_before_any_service_is_started() {
    let (home, run, env) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), TempHome::new());
    let out = Command::new(BIN)
        .envs(env.envs())
        .arg("down")
        .arg(run.path())
        .arg("--home")
        .arg(home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(ServicePaths::new(home.path()).read_info().is_none(), "nothing to run: no service");
}
