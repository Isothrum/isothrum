//! The runner as an embedder drives it, through its public API only: a [`Runner`] on a home of
//! its own, whose tools run through the embedder's own [`ProcessRunner`] (here, a Docker engine
//! the test models), a Compose project's run through its life, its status and its records, a
//! Terraform folder's variables, an adopted deployment, the expired runs, and the errors with
//! their codes and typed detail.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use isothrum_runner::{
    Adoption, BoxFuture, Detail, ErrorCode, Event, Exit, MachineState, OnLine, Park, ProcessError, ProcessRunner, ProcessSpec, RunName, RunOptions, RunState,
    Runner, RunnerConfig, Stream, variables_of,
};

/// A test's outcome: a failed step is its report.
type Outcome<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A Docker engine with Compose, for projects of one service `web`: whether its container is
/// there, and in which state Docker words it.
#[derive(Default)]
struct Engine {
    web: Mutex<Option<&'static str>>,
}

impl Engine {
    /// What `docker <args>` prints, and its exit code.
    fn answer(&self, args: &[&str]) -> (String, i32) {
        let mut web = self.web.lock().unwrap_or_else(PoisonError::into_inner);
        let compose: Vec<&str> = match args {
            ["compose", "version", ..] => return ("2.29.7".into(), 0),
            ["compose", "-p", _, rest @ ..] => rest.iter().copied().filter(|a| *a != "-f" && !Path::new(a).is_absolute()).collect(),
            _ => return (String::new(), 0),
        };
        match compose.first().copied() {
            Some("up") => *web = Some("running"),
            Some("pause") => *web = web.map(|_| "paused"),
            Some("unpause") => *web = web.map(|_| "running"),
            Some("stop") => *web = web.map(|_| "exited"),
            Some("down") => *web = None,
            Some("ps") => {
                let line = web.map(|state| format!(r#"{{"Service":"web","State":"{state}","Health":"","ExitCode":0,"Publishers":[],"Labels":""}}"#));
                return (line.unwrap_or_default(), 0);
            }
            Some("exec") => return ("uid=0(root)".into(), 0),
            Some("config") if compose.get(1) == Some(&"--services") => return ("web".into(), 0),
            Some("config") => return (r#"{"services": {"web": {}}}"#.into(), 0),
            _ => {}
        }
        (String::new(), 0)
    }
}

/// The embedder's process runner: `docker` answered by the engine, every other tool missing.
struct Embedded(Arc<Engine>);

impl ProcessRunner for Embedded {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(async move {
            if spec.program != "docker" {
                return Err(ProcessError::Missing { program: spec.program.clone() });
            }
            let args: Vec<&str> = spec.args.iter().map(String::as_str).collect();
            let (out, code) = self.0.answer(&args);
            for line in out.lines() {
                on_line(Stream::Stdout, line);
            }
            Ok(Exit::new(Some(code)))
        })
    }
}

/// A runner on a fresh home over the modelled engine, and a Compose project beside it.
struct Stack {
    runner: Runner,
    // Folders last: fields drop in order, and on Windows a folder whose files a runner or
    // service still holds open can't be removed.
    _home: tempfile::TempDir,
    project: tempfile::TempDir,
}

fn stack() -> Outcome<Stack> {
    let (home, project) = (tempfile::tempdir()?, tempfile::tempdir()?);
    std::fs::write(project.path().join("compose.yaml"), "services:\n  web: {image: nginx}\n")?;
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::new(Embedded(Arc::default()));
    let runner = Runner::new(config)?;
    Ok(Stack { runner, _home: home, project })
}

/// Says nothing of an event: the test reads the run's state, not its stream.
fn quiet(_: &Event) {}

#[tokio::test]
async fn an_embedder_drives_a_compose_run_through_its_life_and_reads_its_records() -> Outcome {
    let stack = stack()?;
    let name = RunName::new("stack")?;
    let run = stack.runner.open(stack.project.path(), Some(name.clone()))?;
    let opts = RunOptions::new();
    let started = run.up(&opts, &quiet).await?;
    assert_eq!((started.record.name.as_str(), started.record.state), ("stack", RunState::Ready));
    let web = |s: &isothrum_runner::Status| s.machines.iter().map(|m| (m.name.clone(), m.state.clone())).collect::<Vec<_>>();
    let ready = run.status().await?;
    assert_eq!((ready.state, web(&ready)), (RunState::Ready, vec![("web".to_owned(), MachineState::Running)]));

    run.park(Park::Pause, &opts, &quiet).await?;
    assert_eq!(run.status_with(&opts).await?.state, RunState::Parked);
    run.resume(&opts, &quiet).await?;
    let id = run.exec("web", &["id".to_owned()], &opts, &quiet).await?;
    assert_eq!((id.exit, id.stdout.as_str()), (0, "uid=0(root)"));
    run.update(&opts, &quiet).await?;

    let records = stack.runner.runs()?;
    assert_eq!(
        records.iter().map(|r| (r.name.as_str(), r.state)).collect::<Vec<_>>(),
        [("stack", RunState::Ready)]
    );
    let again = stack.runner.reopen(&name)?;
    assert_eq!(again.project().dir, run.project().dir, "the record names its folder");
    assert!(!stack.runner.events(&name)?.events.is_empty(), "its operations' events are kept");

    again.down(&opts, &quiet).await?;
    assert_eq!(run.status().await?.state, RunState::Absent);
    assert_eq!(stack.runner.run_record(&name)?.map(|r| r.state), Some(RunState::Absent));
    Ok(())
}

#[tokio::test]
async fn each_refusal_has_its_code_and_typed_detail() -> Outcome {
    let stack = stack()?;
    let empty = tempfile::tempdir()?;
    let Err(e) = stack.runner.open(empty.path(), None) else {
        return Err("a folder with no project opened".into());
    };
    assert_eq!(e.code(), ErrorCode::ProjectInvalid, "{e}");
    let detail = e.detail().ok_or("no detail")?;
    assert_eq!(
        detail.data,
        Detail::ProjectInvalid {
            dir: empty.path().to_path_buf()
        }
    );

    let run = stack.runner.open(stack.project.path(), None)?;
    let Err(nothing) = run.resume(&RunOptions::new(), &quiet).await else {
        return Err("a run never started resumed".into());
    };
    assert_eq!(nothing.code(), ErrorCode::NothingToResume, "{nothing}");
    let Err(unknown) = stack.runner.reopen(&RunName::new("never")?) else {
        return Err("a run never recorded reopened".into());
    };
    assert_eq!(unknown.code(), ErrorCode::NotRunning, "{unknown}");
    assert!(RunName::new("not a name").is_err_and(|refused| refused.code() == ErrorCode::RunName));
    Ok(())
}

/// A module with one variable of each kind a run's inputs fill, and its state from an apply made
/// outside the runner.
fn module(dir: &Path) -> Outcome {
    let variables = "variable \"region\" {\n  type = string\n  default = \"eu-west-3\"\n}\nvariable \"token\" {\n  type = string\n  sensitive = true\n}\n";
    std::fs::write(dir.join("main.tf"), variables)?;
    Ok(())
}

#[tokio::test]
async fn a_terraform_folder_reads_its_variables_and_an_outside_deployment_is_adopted_then_expires() -> Outcome {
    let stack = stack()?;
    let dir = tempfile::tempdir()?;
    module(dir.path())?;
    let variables = variables_of(dir.path())?;
    let read: BTreeMap<&str, (bool, bool)> = variables.iter().map(|v| (v.name.as_str(), (v.sensitive, v.default.is_some()))).collect();
    assert_eq!(read, BTreeMap::from([("region", (false, true)), ("token", (true, false))]));

    let outside = tempfile::tempdir()?;
    std::fs::write(
        outside.path().join("terraform.tfstate"),
        r#"{"version":4,"serial":1,"lineage":"l","outputs":{},"resources":[]}"#,
    )?;
    let in_an_hour = SystemTime::now() + Duration::from_secs(3600);
    let unix = in_an_hour.duration_since(SystemTime::UNIX_EPOCH)?.as_secs();
    let adoption = Adoption::new(RunName::new("adopted")?, outside.path()).with_expires_at(unix);
    let record = stack.runner.adopt(dir.path(), adoption.clone()).await?;
    assert_eq!((record.state, record.expires_at), (RunState::Ready, Some(unix)));
    assert!(!outside.path().join("terraform.tfstate").exists(), "the state moved into the run");
    let Err(twice) = stack.runner.adopt(dir.path(), adoption).await else {
        return Err("adopted twice".into());
    };
    assert_eq!(twice.code(), ErrorCode::AdoptRefused, "{twice}");

    assert_eq!(stack.runner.expired(SystemTime::now())?, [], "not yet");
    let later = stack.runner.expired(in_an_hour + Duration::from_secs(1))?;
    assert_eq!(later.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["adopted"]);
    Ok(())
}
