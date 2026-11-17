//! What an embedder asks of a home besides operations, through the client alone: a run's record,
//! forgetting it, the runs past their lifetime, the way into a machine, its screen, its
//! hypervisor's app, where its services are reached, what its machines use, the runner's key, and
//! the downloads. The engine is the test's own, so each answer is known; the records are the
//! home's store, written before the service starts.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use isothrum_client::proto::{Command, ConsoleEndpoint, OpOptions, Target};
use isothrum_client::v1::{OperationPhase, phase_of};
use isothrum_client::{Client, ClientError};
use isothrum_runner::store::{OperationId, Store};
use isothrum_runner::{
    BoxFuture, Console, ConsoleProtocol, Desired, Endpoint, ErrorCode, NativeConsole, RunName, RunRecord, RunState, SecretValue, ServiceEndpoint, ShellCommand,
    Status,
};
use isothrum_service::{Engine, Journal, Opened, Refusal, ServeOptions};

mod common;
use common::{Outcome, Serving};

/// An engine whose run `slow` never ends an operation, whose machine `web` has a way in, a screen
/// and an app, and whose key is known. Any other machine has none of them.
struct Machines;

/// `name` as a run's name: a name the tests write, always one.
fn run(name: &str) -> RunName {
    RunName::new(name).unwrap_or_else(|_| RunName::new("unnamed").unwrap_or_else(|_| unreachable!()))
}

/// The refusal for a machine with no console.
fn no_console(machine: &str) -> Refusal {
    Refusal::new(ErrorCode::NoConsole, format!("`{machine}` has no console"))
}

impl Engine for Machines {
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal> {
        Ok(run(target.path.file_name().and_then(|n| n.to_str()).unwrap_or("stack")))
    }

    fn execute<'a>(&'a self, _run: &'a RunName, _command: &'a Command, _operation: OperationId, _journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
        Box::pin(std::future::pending())
    }

    fn status<'a>(&'a self, run: &'a RunName, _target: Option<&'a Target>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async move { Err(Refusal::new(ErrorCode::NotRunning, format!("`{run}` isn't running"))) })
    }

    fn console_command<'a>(&'a self, at: Opened<'a>, machine: &'a str, user: Option<&'a str>) -> BoxFuture<'a, Result<ShellCommand, Refusal>> {
        Box::pin(async move {
            if machine != "web" {
                return Err(no_console(machine));
            }
            let user = user.unwrap_or("root");
            Ok(ShellCommand::new("docker", ["exec", "-it", "-u", user, at.run.as_str()], "/stacks/web"))
        })
    }

    fn graphical_console<'a>(&'a self, _at: Opened<'a>, options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<Console, Refusal>> {
        Box::pin(async move {
            if machine != "web" {
                return Err(no_console(machine));
            }
            let mut console = Console::new(machine, ConsoleProtocol::Vnc, Endpoint::Loopback(SocketAddr::from(([127, 0, 0, 1], 5901))));
            console.password = options.secrets.get("VNC_PASSWORD").map(|p| SecretValue::new(p.clone()));
            Ok(console)
        })
    }

    fn native_console<'a>(&'a self, _at: Opened<'a>, _options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<NativeConsole, Refusal>> {
        Box::pin(async move {
            if machine != "web" {
                return Err(no_console(machine));
            }
            let app = serde_json::json!({"machine": machine, "app": "VirtualBox", "ways": [{"way": "program", "program": "VirtualBox", "args": ["--startvm", "web"]}]});
            serde_json::from_value(app).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
        })
    }

    fn service_endpoints<'a>(&'a self, _at: Opened<'a>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, Refusal>> {
        Box::pin(async {
            let site = serde_json::json!({"name": "web/site", "address": "127.0.0.1:41000", "forwarded": true, "why": null});
            Ok(vec![serde_json::from_value(site).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))?])
        })
    }

    fn usage<'a>(&'a self, _at: Opened<'a>, _options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<isothrum_runner::MachineUsage>, Refusal>> {
        Box::pin(async { Ok(vec![isothrum_runner::MachineUsage::new("web", Some(1.5), Some(10_485_760))]) })
    }

    fn public_key(&self) -> BoxFuture<'_, Result<String, Refusal>> {
        Box::pin(async { Ok("ssh-ed25519 AAAA engine".to_owned()) })
    }

    fn downloads(&self) -> BoxFuture<'_, Result<Vec<isothrum_runner::Download>, Refusal>> {
        Box::pin(async {
            let image = serde_json::json!({"id": 1, "kind": "image", "reference": "nginx:1.27", "image_id": "sha256:a", "run": "web", "fetched_at": "t"});
            Ok(vec![serde_json::from_value(image).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))?])
        })
    }

    fn adopt_downloads<'a>(&'a self, adopted: &'a [isothrum_runner::DownloadToAdopt]) -> BoxFuture<'a, Result<Vec<isothrum_runner::Download>, Refusal>> {
        Box::pin(async move {
            let adopted = adopted.iter().enumerate().filter_map(|(i, a)| match a {
                isothrum_runner::DownloadToAdopt::Image { reference, .. } => {
                    Some(serde_json::json!({"id": i + 2, "kind": "image", "reference": reference, "image_id": "sha256:b", "fetched_at": "t"}))
                }
                _ => None,
            });
            serde_json::from_value(adopted.collect()).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
        })
    }

    fn remove_downloads<'a>(&'a self, which: &'a isothrum_runner::DownloadsToRemove) -> BoxFuture<'a, Result<Vec<isothrum_runner::DownloadRemoval>, Refusal>> {
        Box::pin(async move {
            let isothrum_runner::DownloadsToRemove::Only(ids) = which else {
                return Err(Refusal::new(ErrorCode::Usage, "only by id here"));
            };
            let removed = ids.iter().map(|id| serde_json::json!({"id": id.0, "outcome": "not_recorded"}));
            serde_json::from_value(removed.collect()).map_err(|e| Refusal::new(ErrorCode::Io, e.to_string()))
        })
    }
}

/// A record of run `name` in `state`, which may be removed after `expires_at`.
fn record(name: &str, state: &str, expires_at: u64) -> Outcome<RunRecord> {
    let record = serde_json::json!({
        "version": 1, "name": name, "project": format!("/stacks/{name}"), "format": "compose",
        "state": state, "expires_at": expires_at,
    });
    Ok(serde_json::from_value(record)?)
}

/// A service over [`Machines`] on `home`, whose store records `records` first.
async fn serve(home: &Path, records: &[RunRecord]) -> Outcome<(Client, Serving)> {
    let store = Store::open(home)?;
    for r in records {
        store.write(|tx| tx.save_record(r, Desired::Running))?;
    }
    drop(store);
    common::serve_with(home, ServeOptions::new(Arc::new(Machines), common::never())).await
}

/// The runs' names, in order.
fn names(rows: Vec<isothrum_client::v1::RunRow>) -> Vec<String> {
    rows.into_iter().map(|r| r.name.to_string()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_s_record_is_read_and_forgotten_and_the_runs_past_their_lifetime_listed() {
    let home = tempfile::tempdir().expect("a home");
    let far = 4_102_444_800;
    let records = [record("past", "ready", 1), record("future", "ready", far), record("gone", "absent", 1)].map(|r| r.expect("a record"));
    let (client, _serving) = serve(home.path(), &records).await.expect("a service answering");

    let past = client.record(&run("past")).await.expect("read").expect("recorded");
    assert_eq!((past.state, past.record.expires_at), (RunState::Ready, Some(1)));
    assert!(client.record(&run("never")).await.expect("read").is_none(), "a run never recorded has none");
    assert_eq!(
        names(client.expired().await.expect("listed")),
        ["past"],
        "not the one still to run, nor the one gone"
    );

    client.forget(&run("past")).await.expect("forgotten");
    assert!(client.record(&run("past")).await.expect("read").is_none());
    assert_eq!(names(client.expired().await.expect("listed")), Vec::<String>::new());
    assert_eq!(names(client.list().await.expect("listed")), ["future", "gone"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_run_with_an_operation_under_way_is_not_forgotten() {
    let home = tempfile::tempdir().expect("a home");
    let (client, _serving) = serve(home.path(), &[record("slow", "ready", 1).expect("a record")])
        .await
        .expect("a service answering");
    let up = Command::Up {
        target: Target::new("/stacks/slow", None),
        options: OpOptions::default(),
        lifetime_s: None,
    };
    let accepted = client.submit(up).await.expect("queued");

    let refused = client.forget(&run("slow")).await.expect_err("busy");
    assert_eq!(refused.code(), ErrorCode::Locked, "{refused}");
    assert!(client.record(&run("slow")).await.expect("read").is_some(), "still recorded");

    client.cancel(accepted.operation).await.expect("cancelled");
    let mut events = client.subscribe(Some(run("slow")), None).await.expect("subscribed");
    let ended = async {
        while let Some(Ok((_, event))) = events.next().await {
            if event.operation == Some(accepted.operation) && phase_of(&event) == Some(OperationPhase::Ended) {
                return;
            }
        }
    };
    tokio::time::timeout(common::PATIENCE, ended).await.expect("it ends");
    client.forget(&run("slow")).await.expect("forgotten once nothing runs");
    assert!(client.record(&run("slow")).await.expect("read").is_none(), "forgotten once nothing runs");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_way_into_a_machine_its_screen_its_app_and_the_key_come_from_the_engine() {
    let home = tempfile::tempdir().expect("a home");
    let (client, serving) = serve(home.path(), &[]).await.expect("a service answering");
    let web = run("web");

    let shell = client.console_command(&web, None, "web", Some("ada")).await.expect("a way in");
    assert_eq!(shell, ShellCommand::new("docker", ["exec", "-it", "-u", "ada", "web"], "/stacks/web"));
    let none = client.console_command(&web, None, "db", None).await.expect_err("no way in");
    assert!(matches!(&none, ClientError::Refused(report) if report.code == ErrorCode::NoConsole), "{none:?}");

    let app = client.native_console(&web, None, OpOptions::default(), "web").await.expect("its app");
    assert_eq!((app.app.as_str(), app.ways.len()), ("VirtualBox", 1));
    assert_eq!(client.public_key().await.expect("the key"), "ssh-ed25519 AAAA engine");
    let downloads = client.downloads().await.expect("the downloads");
    assert_eq!(
        downloads.iter().map(|d| (d.id.0, d.run.as_ref().map(RunName::as_str))).collect::<Vec<_>>(),
        [(1, Some("web"))]
    );
    let image = serde_json::from_value(serde_json::json!({"kind": "image", "reference": "kit:3"})).expect("an image to adopt");
    let adopted = client.adopt_downloads(vec![image]).await.expect("adopted");
    assert_eq!(
        adopted.iter().map(|d| (d.id.0, d.run.is_none())).collect::<Vec<_>>(),
        [(2, true)],
        "adopted with no run"
    );
    let which = isothrum_runner::DownloadsToRemove::Only(vec![isothrum_runner::DownloadId(9)]);
    let removed = client.remove_downloads(which).await.expect("asked");
    assert_eq!(
        removed.iter().map(|r| (r.id.0, &r.outcome)).collect::<Vec<_>>(),
        [(9, &isothrum_runner::RemovalOutcome::NotRecorded)]
    );
    let refused = client.remove_downloads(isothrum_runner::DownloadsToRemove::All).await.expect_err("refused");
    assert_eq!(refused.code(), ErrorCode::Usage, "{refused}");
    let endpoints = client.service_endpoints(&web, None, OpOptions::default()).await.expect("its endpoints");
    assert_eq!(
        endpoints
            .iter()
            .map(|e| (e.name.as_str(), e.address.as_deref(), e.forwarded))
            .collect::<Vec<_>>(),
        [("web/site", Some("127.0.0.1:41000"), true)]
    );
    let used = client.usage(&web, None, OpOptions::default()).await.expect("what it uses");
    assert_eq!(used, [isothrum_runner::MachineUsage::new("web", Some(1.5), Some(10_485_760))]);

    let options = OpOptions {
        secrets: [("VNC_PASSWORD".to_owned(), "s3cret".to_owned())].into(),
        ..OpOptions::default()
    };
    let mut held = client.graphical_console(&web, None, options, "web").await.expect("its screen");
    let at = ConsoleEndpoint::Loopback {
        address: SocketAddr::from(([127, 0, 0, 1], 5901)),
    };
    assert_eq!(
        (&held.console.endpoint, held.console.password.as_deref(), held.console.tunnelled),
        (&at, Some("s3cret"), false)
    );
    assert!(!format!("{held:?}").contains("s3cret"), "{held:?}");
    let screenless = client.graphical_console(&web, None, OpOptions::default(), "db").await.expect_err("none");
    assert_eq!(screenless.code(), ErrorCode::NoConsole, "{screenless}");

    // A console reached through no tunnel ends only with the service: its connection closes.
    client.shutdown().await.expect("shut down");
    let reason = tokio::time::timeout(common::PATIENCE, held.closed()).await.expect("in time");
    assert!(reason.contains("closed the connection"), "{reason}");
    common::ended(serving).await.expect("the service ended");
}
