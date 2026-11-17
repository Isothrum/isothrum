//! The engine self-test: a throwaway two-container deployment, run through a runner of its own.
//! It checks the engine answers, its test image is here (pulled when it isn't), the deployment
//! starts, its containers reach each other by name on their network, and a port it publishes
//! answers on this computer. The deployment is always taken down.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::{DownloadToAdopt, RunOptions, Runner};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{Progress, throwaway};
use crate::Context;
use crate::docker::{NoAnswer, daemon_info, docker};

/// The test's image: small, with a shell, `wget` and `httpd`.
pub const IMAGE: &str = "busybox:1.36";
/// What the test's web server serves, so a reply is known to come from it.
const MARKER: &str = "isothrum-selftest-ok";
/// The run's name in the test's own runner.
const RUN: &str = "engine-selftest";
/// Tries, [`RETRY`] apart, for the web server to answer: `httpd` needs a moment.
const ANSWER_TRIES: u32 = 10;
const RETRY: Duration = Duration::from_millis(500);
/// How long the test image's pull may take.
const PULL_LIMIT: Duration = Duration::from_secs(300);
/// How long a reply to the port's request may take.
const REPLY_LIMIT: Duration = Duration::from_secs(3);

/// A step of the engine self-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
    /// The engine answers.
    Engine,
    /// The test image is here, pulled when it wasn't.
    Pull,
    /// The test deployment starts.
    Start,
    /// Its containers reach each other by name.
    Network,
    /// A port it publishes answers on this computer.
    Port,
}

/// What a step that passed found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
    /// The engine's version.
    Version(String),
    /// The test image: whether it was here already.
    Image {
        /// Here before the test (nothing pulled).
        held: bool,
    },
    /// Nothing to say.
    Nothing,
    /// One container reached the other by name.
    Reached,
    /// Where the published port answered (`host:port`).
    Address(String),
}

/// Why a step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// The engine gave no answer.
    Engine(NoAnswer),
    /// What the tool or the runner said.
    Said(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Engine(why) => why.fmt(f),
            Failure::Said(said) => f.write_str(said),
        }
    }
}

/// A step's progress, as the test says it.
pub type StepProgress = Progress<Step, Found, Failure>;

/// How the test went.
#[derive(Debug)]
#[non_exhaustive]
pub struct Ran {
    /// Whether every step passed; why not (the failed step's reason, or what kept the test from
    /// starting).
    pub passed: Result<(), Failure>,
    /// Whether the test deployment was taken down; why not.
    pub cleaned_up: Result<(), String>,
    /// The test image as this test pulled it, when it did: for a caller that keeps a record of
    /// its downloads.
    pub pulled: Option<DownloadToAdopt>,
}

/// The test deployment: a web server on a loopback port serving its own host name, [`MARKER`]
/// (`/hostname`), and a prober.
fn compose_file() -> String {
    format!(
        r#"services:
  web:
    image: {IMAGE}
    hostname: {MARKER}
    command: ["httpd", "-f", "-p", "80", "-h", "/etc"]
    ports: ["127.0.0.1::80"]
  probe:
    image: {IMAGE}
    command: ["sleep", "300"]
"#
    )
}

/// The id `IMAGE` names on the engine `context` reaches; none when it holds no such image.
async fn image_id(cx: &Context, context: Option<&str>) -> Option<String> {
    let spec = docker(context, &["image", "inspect", "--format", "{{.Id}}", IMAGE]);
    let out = cx.probe_within(cx.probe_timeout, &spec).await.ok()?.output().ok()?;
    Some(out.trim().to_owned()).filter(|id| !id.is_empty())
}

/// Pulls the test image on the engine `context` reaches unless it is there: whether it was, and
/// the image as pulled when it wasn't. Setup may call it ahead of the test, so the test finds the
/// image there.
///
/// # Errors
///
/// The pull failed: what the engine said.
pub async fn pull(cx: &Context, context: Option<&str>) -> Result<(bool, Option<DownloadToAdopt>), String> {
    if image_id(cx, context).await.is_some() {
        return Ok((true, None));
    }
    let spec = docker(context, &["pull", "--quiet", IMAGE]);
    let pulled = cx.probe_within(PULL_LIMIT, &spec).await.map_err(|e| e.to_string())?;
    pulled.output().map_err(|e| e.reason)?;
    let image_id = image_id(cx, context).await;
    Ok((
        false,
        Some(DownloadToAdopt::Image {
            reference: IMAGE.to_owned(),
            image_id,
        }),
    ))
}

/// Runs the test on the engine Docker context `context` reaches (the current one when none is
/// named), its deployment in `dir` (the test's own folder: its runner's home and project are
/// there), each step's progress said to `progress`.
pub async fn run(cx: &Context, dir: &Path, context: Option<&str>, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Ran {
    let mut pulled = None;
    let made = throwaway(cx, dir, "compose.yaml", &compose_file());
    let (runner, project) = match made {
        Ok(test) => test,
        Err(e) => {
            return Ran {
                passed: Err(Failure::Said(e)),
                cleaned_up: Ok(()),
                pulled,
            };
        }
    };
    let opts = context.map_or_else(RunOptions::new, |c| RunOptions::new().with_docker_context(c));
    let test = Test {
        cx,
        runner: &runner,
        project: &project,
        context,
        opts: &opts,
    };
    let passed = Box::pin(steps(&test, &mut pulled, progress)).await;
    let cleaned_up = match super::open(&runner, &project, RUN) {
        Ok(run) => run.down(&opts, &super::quiet).await.map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    Ran { passed, cleaned_up, pulled }
}

/// What the steps work on: the engine, the test's runner, its project and options.
struct Test<'a> {
    cx: &'a Context,
    runner: &'a Runner,
    project: &'a Path,
    context: Option<&'a str>,
    opts: &'a RunOptions,
}

/// The steps, in order: the first that fails ends the test.
async fn steps(test: &Test<'_>, pulled: &mut Option<DownloadToAdopt>, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Result<(), Failure> {
    let Test {
        cx,
        runner,
        project,
        context,
        opts,
    } = *test;
    let step = super::Steps(progress);
    step.run(Step::Engine, async {
        daemon_info(cx, "{{.ServerVersion}}", context)
            .await
            .map(|v| Found::Version(v.trim().to_owned()))
            .map_err(Failure::Engine)
    })
    .await?;
    step.run(Step::Pull, async {
        let (held, image) = pull(cx, context).await.map_err(Failure::Said)?;
        *pulled = image;
        Ok(Found::Image { held })
    })
    .await?;
    let run = super::open(runner, project, RUN).map_err(Failure::Said)?;
    let up = Box::pin(async {
        run.up(opts, &super::quiet)
            .await
            .map(|_| Found::Nothing)
            .map_err(|e| Failure::Said(e.to_string()))
    });
    step.run(Step::Start, up).await?;
    step.run(Step::Network, async { reach_by_name(&run, opts).await.map_err(Failure::Said) })
        .await?;
    step.run(Step::Port, async { reach_port(&run, opts).await.map_err(Failure::Said) }).await?;
    Ok(())
}

/// The prober reaches the web server by name, on the deployment's network.
async fn reach_by_name(run: &isothrum_runner::Running<'_>, opts: &RunOptions) -> Result<Found, String> {
    let argv = ["wget", "-qO-", "-T", "2", "http://web/hostname"].map(str::to_owned);
    let mut last = String::new();
    for _ in 0..ANSWER_TRIES {
        match run.exec("probe", &argv, opts, &super::quiet).await {
            Ok(ran) if ran.stdout.contains(MARKER) => return Ok(Found::Reached),
            Ok(ran) => format!("{} {}", ran.stdout.trim(), ran.stderr.trim()).trim().clone_into(&mut last),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(RETRY).await;
    }
    Err(format!("probe could not reach web: {last}"))
}

/// The web server's published port answers on this computer's loopback.
async fn reach_port(run: &isothrum_runner::Running<'_>, opts: &RunOptions) -> Result<Found, String> {
    let status = run.status_with(opts).await.map_err(|e| e.to_string())?;
    let port = status
        .machines
        .iter()
        .find(|m| m.name == "web")
        .and_then(|m| m.published.first())
        .map(|p| p.host_port.get());
    let address = format!("127.0.0.1:{}", port.ok_or("web publishes no port")?);
    let mut last = String::new();
    for _ in 0..ANSWER_TRIES {
        match get(&address).await {
            Ok(body) if body.contains(MARKER) => return Ok(Found::Address(address)),
            Ok(body) => last = body,
            Err(e) => last = e,
        }
        tokio::time::sleep(RETRY).await;
    }
    Err(format!("http://{address} did not answer: {last}"))
}

/// What `GET /hostname` on `address` answers, within [`REPLY_LIMIT`].
async fn get(address: &str) -> Result<String, String> {
    let ask = async {
        let mut stream = tokio::net::TcpStream::connect(address).await?;
        stream.write_all(b"GET /hostname HTTP/1.0\r\nHost: localhost\r\n\r\n").await?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await?;
        Ok::<_, std::io::Error>(String::from_utf8_lossy(&reply).into_owned())
    };
    match tokio::time::timeout(REPLY_LIMIT, ask).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("no reply within {}s", REPLY_LIMIT.as_secs())),
    }
}

#[cfg(test)]
mod tests;
