//! Every command a client sends, through a real service on a fresh home, over a runner whose
//! Docker is a model: each one's reply, and a refusal's typed detail (`v`: 1, its kind).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use isothrum_client::proto::{Command, OpOptions, PROTOCOL, Reply, Target};
use isothrum_client::{Client, ClientError, VERSION};
use isothrum_runner::fake::{Call, FakeProcesses, Reply as Answer};
use isothrum_runner::{Detail, ErrorCode, Park, RunName, RunState};
use isothrum_runner::{Runner, RunnerConfig};
use isothrum_service::{RunnerEngine, ServeOptions};
use std::sync::Arc;

mod common;
use common::Outcome;

/// A service on `home` over a runner whose tools `answer` answers (a model of them), never
/// stopped but by a request; a client of it once it answers.
async fn serve(home: &Path, answer: impl Fn(&Call) -> Answer + Send + Sync + 'static) -> Outcome<(Client, common::Serving)> {
    let mut config = RunnerConfig::new(home);
    config.processes = Arc::new(FakeProcesses::model(answer));
    let engine = Arc::new(RunnerEngine::new(Arc::new(Runner::new(config)?)));
    common::serve_with(home, ServeOptions::new(engine, common::never())).await
}

/// Docker with Compose, for a project of `web` and `attackbox` (under a profile): the services
/// with a container, and whether each runs.
#[derive(Default)]
struct Docker {
    containers: Mutex<BTreeMap<String, bool>>,
}

impl Docker {
    fn answer(&self, call: &Call) -> Answer {
        let mut containers = self.containers.lock().unwrap_or_else(PoisonError::into_inner);
        let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
        let rest = match args.as_slice() {
            ["compose", "version", "--short"] => return Answer::ok("2.29.7"),
            ["compose", "-p", _, rest @ ..] => rest,
            _ => return Answer::ok(""),
        };
        let words: Vec<&str> = rest.iter().copied().filter(|a| *a != "-f" && !Path::new(a).is_absolute()).collect();
        let named: Vec<&str> = words.iter().copied().filter(|w| ["web", "attackbox"].contains(w)).collect();
        match words.first().copied() {
            Some("up") => {
                for service in if named.is_empty() { vec!["web"] } else { named } {
                    containers.insert(service.to_owned(), true);
                }
                Answer::ok("")
            }
            Some("pause" | "stop") => {
                containers.values_mut().for_each(|running| *running = false);
                Answer::ok("")
            }
            Some("rm") => {
                for service in named {
                    containers.remove(service);
                }
                Answer::ok("")
            }
            Some("down") => {
                containers.clear();
                Answer::ok("")
            }
            Some("ps") => {
                let lines: Vec<String> = containers
                    .iter()
                    .map(|(service, running)| {
                        let state = if *running { "running" } else { "exited" };
                        format!(r#"{{"Service":"{service}","State":"{state}","Health":"","ExitCode":0,"Publishers":[],"Labels":""}}"#)
                    })
                    .collect();
                Answer::ok(&lines.join("\n"))
            }
            Some("exec") => Answer::ok("ran"),
            Some("config") if words.get(1) == Some(&"--services") => Answer::ok("web\n"),
            Some("config") => Answer::ok(r#"{"services": {"web": {}}}"#),
            _ => Answer::ok(""),
        }
    }
}

/// A service over a runner on the modelled Docker, on a fresh home, a Compose project beside it.
struct Lab {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    client: Client,
    task: common::Serving,
}

async fn lab() -> Outcome<Lab> {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    std::fs::write(
        project.path().join("compose.yaml"),
        "services:\n  web: {image: nginx}\n  attackbox: {image: kali, profiles: [attack]}\n",
    )?;
    let docker = Docker::default();
    let (client, task) = serve(home.path(), move |call| docker.answer(call)).await?;
    Ok(Lab { home, project, client, task })
}

impl Lab {
    fn target(&self) -> Target {
        Target::new(self.project.path(), RunName::new("lab").ok())
    }

    /// The operation `submitted` accepted, waited for to its end.
    async fn ran(&self, submitted: Result<isothrum_client::Accepted, ClientError>) -> Result<(), ClientError> {
        let accepted = submitted?;
        assert_eq!(accepted.run.as_str(), "lab");
        self.client.wait(accepted.operation).await.map(drop)
    }

    /// Ends the service with `shutdown_if_idle` through `client`, and waits until it is gone.
    async fn end(self, client: &Client) {
        assert!(matches!(client.request(Command::ShutdownIfIdle).await, Ok(Reply::Ok { .. })));
        common::ended(self.task).await.expect("the service ended");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_operation_a_client_submits_runs_to_its_end() {
    let lab = lab().await.unwrap();
    let (client, target, name) = (&lab.client, lab.target(), RunName::new("lab").unwrap());
    let none = OpOptions::default;
    lab.ran(client.up(target.clone(), none()).await).await.expect("up");
    lab.ran(client.park(target.clone(), none(), Park::Pause).await).await.expect("park");
    assert_eq!(client.status(&name, None).await.expect("status").state, RunState::Parked);
    lab.ran(client.resume(target.clone(), none()).await).await.expect("resume");
    lab.ran(client.update(target.clone(), none()).await).await.expect("update");
    let attackbox = || "attackbox".to_owned();
    lab.ran(client.attach(target.clone(), none(), attackbox()).await).await.expect("attach");
    let argv = vec!["true".to_owned()];
    lab.ran(client.exec(target.clone(), none(), "web".to_owned(), argv).await).await.expect("exec");
    lab.ran(client.retire(target.clone(), none(), attackbox()).await).await.expect("retire");
    let provision = Command::Provision {
        target: target.clone(),
        options: none(),
        machine: None,
        with: None,
    };
    let unprovisioned = lab.ran(client.submit(provision).await).await.expect_err("Compose has no provisioning");
    assert!(
        matches!(
            &unprovisioned,
            ClientError::Operation {
                code: ErrorCode::Unsupported,
                ..
            }
        ),
        "{unprovisioned:?}"
    );
    lab.ran(client.down(target.clone(), none()).await).await.expect("down");
    assert_eq!(client.status(&name, None).await.expect("status").state, RunState::Absent);
    let second = Client::connect_running(lab.home.path()).await.expect("a second client");
    lab.end(&second).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_query_gets_its_reply_and_a_refusal_its_typed_detail() {
    let lab = lab().await.unwrap();
    let (client, target, name) = (&lab.client, lab.target(), RunName::new("lab").unwrap());
    // hello, on the way in; a client of this release connects as it is, replacing nothing.
    assert_eq!((client.service().protocol, client.service().version.as_str()), (PROTOCOL, VERSION));
    let exact = Client::connect_exact(lab.home.path(), None).await.expect("this release's own service");
    assert_eq!(exact.service().pid, client.service().pid, "the same service, not another started");

    let mut events = client.subscribe(Some(name.clone()), None).await.expect("subscribed");
    lab.ran(client.up(target.clone(), OpOptions::default()).await).await.expect("up");
    assert_eq!(client.status(&name, None).await.expect("status").state, RunState::Ready);
    let revealing = OpOptions {
        reveal: true,
        ..OpOptions::default()
    };
    assert_eq!(client.status_with(&name, None, revealing).await.expect("status").state, RunState::Ready);
    assert!(client.list().await.expect("list").iter().any(|r| r.name == name), "the run is listed");
    let operations = client.operations(&name, 10).await.expect("operations");
    let (_, first) = tokio::time::timeout(common::PATIENCE, events.next())
        .await
        .expect("an event in time")
        .expect("the subscription")
        .expect("an event");
    assert_eq!(first.run.as_deref(), Some("lab"), "{first:?}");
    assert!(matches!(client.request(Command::Unsubscribe).await.expect("unsubscribe"), Reply::Ok { .. }));

    // A refusal before anything is queued carries the runner's typed detail.
    let refused = client.variables(target.clone()).await.expect_err("no Terraform module");
    let ClientError::Refused {
        code, detail: Some(detail), ..
    } = &refused
    else {
        panic!("{refused:?}")
    };
    assert_eq!((*code, detail.v), (ErrorCode::ProjectInvalid, 1));
    assert!(
        matches!(&detail.data, Detail::ProjectInvalid { dir } if dir == lab.project.path()),
        "{detail:?}"
    );
    let ended = operations.first().map(|o| o.id).expect("the up, journaled");
    let cancel = client.cancel(ended).await.expect_err("it ended");
    assert_eq!(cancel.code(), ErrorCode::Usage, "{cancel}");
    lab.end(&exact).await;
}
