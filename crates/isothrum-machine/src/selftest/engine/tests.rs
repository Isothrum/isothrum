//! The engine self-test on a modelled engine: each step said in order, the image pulled only
//! when it isn't there and handed back, the deployment taken down after a failure too.

use std::sync::{Arc, Mutex, PoisonError};

use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{DownloadToAdopt, ProcessRunner};

use super::*;
use crate::testing::linux_host;

/// The modelled engine: whether it answers, whether it holds the image, and the port its web
/// server's answer is published on.
struct Engine {
    answers: bool,
    held: bool,
    port: u16,
}

fn engine(e: &Mutex<Engine>, call: &Call) -> Reply {
    let mut e = e.lock().unwrap_or_else(PoisonError::into_inner);
    let line = call.args.join(" ");
    if !e.answers && line.contains("info") {
        return Reply::fail(
            1,
            "Cannot connect to the Docker daemon at unix:///var/run/docker.sock. Is the docker daemon running?",
        );
    }
    if line.contains("info --format") {
        Reply::ok("27.3.1")
    } else if line.contains("image inspect") {
        if e.held {
            Reply::ok("sha256:busybox")
        } else {
            Reply::fail(1, "Error: No such image: busybox:1.36")
        }
    } else if line.contains("pull --quiet") {
        e.held = true;
        Reply::ok("docker.io/library/busybox:1.36")
    } else if line.contains("compose version") {
        Reply::ok("2.29.7")
    } else if line.contains("config --format json") {
        Reply::ok(
            r#"{"services": {"web": {"image": "busybox:1.36", "ports": [{"target": 80, "host_ip": "127.0.0.1", "protocol": "tcp"}]}, "probe": {"image": "busybox:1.36"}}}"#,
        )
    } else if line.contains(" ps ") {
        let web = format!(
            r#"{{"Service":"web","State":"running","Health":"","ExitCode":0,"Publishers":[{{"URL":"127.0.0.1","TargetPort":80,"PublishedPort":{},"Protocol":"tcp"}}],"Labels":""}}"#,
            e.port
        );
        Reply::ok(&format!(
            "{web}\n{}",
            r#"{"Service":"probe","State":"running","Health":"","ExitCode":0,"Publishers":[],"Labels":""}"#
        ))
    } else if line.contains("exec") {
        Reply::ok(MARKER)
    } else {
        Reply::ok("")
    }
}

/// A web server on this computer's loopback serving [`MARKER`]: the test deployment's port.
async fn web() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let port = listener.local_addr().expect("its address").port();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut ask = [0_u8; 512];
            drop(stream.read(&mut ask).await);
            drop(stream.write_all(format!("HTTP/1.0 200 OK\r\n\r\n{MARKER}\n").as_bytes()).await);
        }
    });
    port
}

/// The test on the modelled engine: the steps said, how it ran, and the calls made.
async fn tested(answers: bool, held: bool) -> (Vec<StepProgress>, Ran, Arc<FakeProcesses>) {
    let port = web().await;
    let state = Mutex::new(Engine { answers, held, port });
    let tools = Arc::new(FakeProcesses::model(move |call| engine(&state, call)));
    let cx = Context::new(
        {
            let runner: Arc<dyn ProcessRunner> = Arc::<FakeProcesses>::clone(&tools);
            runner
        },
        linux_host(Path::new("/nonexistent/isothrum-machine-test")),
    );
    let dir = tempfile::tempdir().expect("a folder");
    let said = Mutex::new(Vec::new());
    let sink = |p: StepProgress| said.lock().unwrap_or_else(PoisonError::into_inner).push(p);
    let ran = run(&cx, dir.path(), Some("orbstack"), &sink).await;
    let said = said.into_inner().unwrap_or_else(PoisonError::into_inner);
    (said, ran, tools)
}

#[tokio::test]
async fn every_step_passes_in_order_and_the_image_pulled_is_handed_back() {
    let (said, ran, tools) = tested(true, false).await;
    let passed: Vec<(Step, Found)> = said
        .into_iter()
        .filter_map(|p| match p {
            Progress::Passed(step, found) => Some((step, found)),
            _ => None,
        })
        .collect();
    assert_eq!(
        passed.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
        [Step::Engine, Step::Pull, Step::Start, Step::Network, Step::Port]
    );
    assert_eq!(passed[0].1, Found::Version("27.3.1".into()));
    assert_eq!(passed[1].1, Found::Image { held: false });
    assert!(matches!(&passed[4].1, Found::Address(a) if a.starts_with("127.0.0.1:")), "{passed:?}");
    assert_eq!((ran.passed, ran.cleaned_up), (Ok(()), Ok(())));
    assert_eq!(
        ran.pulled,
        Some(DownloadToAdopt::Image {
            reference: IMAGE.into(),
            image_id: Some("sha256:busybox".into())
        })
    );
    let calls: Vec<String> = tools.calls().iter().map(|c| c.args.join(" ")).collect();
    assert!(calls.iter().any(|c| c.contains("compose") && c.contains(" down")), "taken down: {calls:?}");
}

#[tokio::test]
async fn a_held_image_is_not_pulled_and_a_failed_step_ends_the_test_which_still_cleans_up() {
    let (_, ran, tools) = tested(true, true).await;
    assert!(ran.pulled.is_none());
    assert!(
        tools
            .calls()
            .iter()
            .all(|c| !c.args.contains(&"pull".to_owned()) || c.args.contains(&"compose".to_owned()))
    );

    let (said, down, engine_tools) = tested(false, true).await;
    assert!(
        matches!(said.as_slice(), [Progress::Running(Step::Engine), Progress::Failed(Step::Engine, why)] if why.to_string().contains("Cannot connect")),
        "{said:?}"
    );
    assert!(down.passed.is_err());
    assert!(
        engine_tools.calls().iter().any(|c| c.args.join(" ").contains(" down")),
        "taken down all the same"
    );
}
