//! A Compose run one of whose services is removed outside it (`docker rm -f web`), or while it
//! starts.

use super::*;
use crate::testing::modelled;

/// What the engine holds of the project: the services with a container, how many `up`s it saw,
/// and whether an `up` loses `web` (removed while the run starts).
#[derive(Debug, Default)]
pub(super) struct Stack {
    pub(super) present: Vec<&'static str>,
    ups: usize,
    loses_web: bool,
}

/// Docker with Compose, for a project of two services, `web` and `cache`.
fn answer(stack: &Mutex<Stack>, call: &Call) -> Reply {
    let mut stack = stack.lock().unwrap_or_else(PoisonError::into_inner);
    let args: Vec<&str> = call.args.iter().map(String::as_str).collect();
    let rest = match args.as_slice() {
        ["compose", "version", "--short"] => return Reply::ok("2.29.7"),
        ["compose", "-p", _, "-f", _, rest @ ..] => rest,
        _ => return Reply::ok(""),
    };
    match rest {
        ["up", ..] => {
            stack.ups += 1;
            stack.present = if stack.loses_web { vec!["cache"] } else { vec!["cache", "web"] };
            Reply::ok("")
        }
        ["ps", "--all", "--format", "json"] => {
            let lines: Vec<String> = stack
                .present
                .iter()
                .map(|s| format!(r#"{{"Service":"{s}","State":"running","Health":"","ExitCode":0,"Publishers":[],"Labels":""}}"#))
                .collect();
            Reply::ok(&lines.join("\n"))
        }
        ["config", "--services"] => Reply::ok("web\ncache\n"),
        ["config", "--format", "json"] => Reply::ok(r#"{"services": {"web": {}, "cache": {}}}"#),
        ["down", ..] => {
            stack.present.clear();
            Reply::ok("")
        }
        _ => Reply::ok(""),
    }
}

/// A Compose project, a runner on the modelled engine, and the engine.
pub(super) fn stack() -> (tempfile::TempDir, std::path::PathBuf, Runner, Arc<Mutex<Stack>>) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("stack");
    std::fs::create_dir(&project).expect("the project");
    std::fs::write(project.join("compose.yaml"), "services:\n  web: {image: nginx}\n  cache: {image: redis}\n").expect("a compose file");
    let (stack, fake) = modelled::<Stack>(answer);
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    (root, project, Runner::new(config).expect("a runner"), stack)
}

#[tokio::test]
async fn a_removed_service_reads_absent_its_resume_is_refused_naming_it_and_an_up_makes_it_again() {
    let (_root, project, runner, stack) = stack();
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    stack.lock().unwrap_or_else(PoisonError::into_inner).present.retain(|s| *s != "web");

    let status = run.status().await.expect("status");
    let web = status.machines.iter().find(|m| m.name == "web").expect("web is listed");
    assert_eq!((status.state, &web.state), (RunState::Failed, &MachineState::Absent));

    refused_as_gone(&run.resume(&RunOptions::new(), &Recorder::new()).await.expect_err("web is gone"));
    assert_eq!(stack.lock().unwrap_or_else(PoisonError::into_inner).ups, 1, "nothing was started");

    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up makes it again");
    assert_eq!(stack.lock().unwrap_or_else(PoisonError::into_inner).ups, 2);
    assert_eq!(run.status().await.expect("status").state, RunState::Ready);
}

#[tokio::test]
async fn a_service_removed_while_the_run_starts_leaves_it_not_ready_never_whole_but_smaller() {
    let (_root, project, runner, stack) = stack();
    stack.lock().unwrap_or_else(PoisonError::into_inner).loses_web = true;
    let run = runner.open(&project, None).expect("opened");
    let e = run.up(&RunOptions::new(), &Recorder::new()).await.expect_err("web went while it started");
    assert_eq!(e.code(), ErrorCode::NotReady, "{e}");
    assert!(matches!(&e, crate::Error::NotReady { detail, .. } if detail == "`web` absent"), "{e:?}");
    let status = run.status().await.expect("status");
    assert_ne!(status.state, RunState::Ready);
    let web = status.machines.iter().find(|m| m.name == "web").expect("web is listed");
    assert_eq!(web.state, MachineState::Absent);
}
