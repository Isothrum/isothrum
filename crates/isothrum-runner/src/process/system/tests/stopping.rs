//! A tool cut short: asked to stop, given its grace, then ended, with what it started.

use isothrum_test_support::alive;

use super::*;
#[cfg(unix)]
use crate::process::stopping::Stopping;

/// Whether the process `pid` guards is gone within half a minute (a loaded host reaps slowly).
async fn gone(pid: &PidGuard) -> bool {
    for _ in 0..1500 {
        if !alive(pid.id()) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Runs `job` until it wrote a pid to `file`, then cancels it (drops the run), however long it
/// took to get there: the process it wrote, guarded (it is killed when the test ends, whatever
/// way, so a failed test leaves nothing running).
async fn cancelled_once_started(runner: &SystemProcesses, job: &ProcessSpec, file: &std::path::Path) -> PidGuard {
    let mut run = Box::pin(capture(runner, job));
    let started = async {
        loop {
            if let Some(pid) = PidGuard::from_file(file) {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(300), async {
        tokio::select! {
            out = &mut run => panic!("the tool ended before it was cancelled: {out:?}"),
            pid = started => pid,
        }
    })
    .await
    .expect("the tool never started")
}

#[tokio::test]
async fn a_tool_cancelled_takes_what_it_started_with_it() {
    // Regression: a timeout killed the tool alone, and what it had started (a wrapper's real
    // program, `vagrant`'s ruby) ran on, holding Vagrant's lock.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pid");
    let path = file.display().to_string();
    let job = if cfg!(windows) {
        let script = format!(
            "$p = Start-Process -FilePath ping -ArgumentList '-n','120','127.0.0.1' -PassThru -NoNewWindow; Set-Content -Path '{path}' -Value $p.Id; Start-Sleep 120"
        );
        spec("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script])
    } else {
        spec("sh", &["-c", &format!("sleep 120 & echo $! > '{path}'; wait")])
    };
    let runner = SystemProcesses::default();
    // Cancelled once its child runs (its pid written), however long the tool took to get there:
    // a PowerShell's cold start on a busy Windows runner took longer than any fixed wait.
    let child = cancelled_once_started(&runner, &job, &file).await;
    assert!(gone(&child).await, "the tool's child {} was left running", child.id());
}

/// A `sh` job that writes its pid to `pid`, then loops until stopped (a minute at most, should
/// the test binary itself be killed before its guard ran); `on_int` is its SIGINT trap (`''` to
/// ignore it).
#[cfg(unix)]
fn looping(pid: &std::path::Path, on_int: &str) -> ProcessSpec {
    let script = format!(
        "trap {on_int} INT; echo $$ > '{}'; i=0; while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done",
        pid.display()
    );
    spec("sh", &["-c", &script])
}

/// A tool cut short is asked to stop (SIGINT to its group), as Terraform needs to save its state,
/// and the processes wait for it.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_cut_short_is_asked_to_stop_before_it_is_ended() {
    let dir = tempfile::tempdir().unwrap();
    let (pid, said) = (dir.path().join("pid"), dir.path().join("said"));
    let job = looping(&pid, &format!("\"echo stopped > '{}'; exit 0\"", said.display()));
    let runner = SystemProcesses::default();
    let tool = cancelled_once_started(&runner, &job, &pid).await;
    tokio::time::timeout(Duration::from_secs(10), runner.stopped())
        .await
        .expect("it stopped on its own");
    assert!(!runner.is_stopping());
    assert_eq!(std::fs::read_to_string(&said).unwrap().trim(), "stopped", "its interrupt handler ran");
    assert!(gone(&tool).await);
}

/// A tool run in a container (a `docker run` client) that is ended has its container removed:
/// ending the client alone left the container, and the Terraform in it, running.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_ended_has_its_container_removed() {
    let dir = tempfile::tempdir().unwrap();
    let (pid, log, docker) = (dir.path().join("pid"), dir.path().join("log"), dir.path().join("docker"));
    // A `docker` that, run, ignores the request to stop (for a minute at most); asked to remove,
    // says what it was asked. Written by a child process, as every program a test runs.
    let script = format!(
        r#"#!/bin/sh
if [ "$1" = rm ]; then echo "$*" >> '{}'; exit 0; fi
trap '' INT; echo $$ > '{}'; i=0; while [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
"#,
        log.display(),
        pid.display()
    );
    isothrum_test_support::write_executable(&docker, &script).unwrap();
    let mut job = spec(&docker.display().to_string(), &["run", "--rm", "-i", "--name", "isothrum-tool-1", "img"]);
    job.container = Some("isothrum-tool-1".into());
    let runner = SystemProcesses {
        stopping: Arc::new(Stopping::new(Duration::from_secs(1))),
        ..SystemProcesses::default()
    };
    // Cancelled once it runs (its pid written), however long a loaded host took to start it.
    let tool = cancelled_once_started(&runner, &job, &pid).await;
    tokio::time::timeout(Duration::from_secs(10), runner.stopped())
        .await
        .expect("ended after its grace");
    assert!(gone(&tool).await);
    assert_eq!(std::fs::read_to_string(&log).unwrap().trim(), "rm -f isothrum-tool-1");
}

/// A tool that doesn't stop when asked is ended once its grace is over.
#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_ignores_the_request_is_ended_after_its_grace() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let runner = SystemProcesses {
        stopping: Arc::new(Stopping::new(Duration::from_secs(1))),
        ..SystemProcesses::default()
    };
    let tool = cancelled_once_started(&runner, &looping(&pid, "''"), &pid).await;
    assert!(runner.is_stopping(), "given its grace");
    tokio::time::timeout(Duration::from_secs(10), runner.stopped())
        .await
        .expect("ended after its grace");
    assert!(gone(&tool).await);
}

/// A second Ctrl-C: the tools still stopping are ended at once.
#[cfg(unix)]
#[tokio::test]
async fn the_tools_still_stopping_can_be_ended_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let runner = SystemProcesses::default();
    let tool = cancelled_once_started(&runner, &looping(&pid, "''"), &pid).await;
    runner.end_stopping();
    tokio::time::timeout(Duration::from_secs(5), runner.stopped())
        .await
        .expect("ended well before its 30 s");
    assert!(gone(&tool).await);
}

/// Regression (#132): on Windows a tool cut short was terminated at once, so a `terraform apply`
/// cancelled mid-way lost the state of what it had made. It is sent Ctrl-C on its console first:
/// a PowerShell loop's `finally` runs.
#[cfg(windows)]
#[tokio::test]
async fn a_tool_cut_short_on_windows_gets_ctrl_c_before_it_is_ended() {
    let dir = tempfile::tempdir().unwrap();
    let (pid, said) = (dir.path().join("pid"), dir.path().join("said"));
    let script = format!(
        "try {{ Set-Content -LiteralPath '{}' $PID; while ($true) {{ Start-Sleep -Milliseconds 100 }} }} finally {{ Set-Content -LiteralPath '{}' 'stopped' }}",
        pid.display(),
        said.display()
    );
    let job = spec("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script]);
    let runner = SystemProcesses::default();
    let started = cancelled_once_started(&runner, &job, &pid).await;
    tokio::time::timeout(Duration::from_secs(40), runner.stopped()).await.expect("it stopped");
    assert_eq!(
        std::fs::read_to_string(&said).map(|s| s.trim().to_owned()).ok().as_deref(),
        Some("stopped"),
        "its Ctrl-C handler ran"
    );
    assert!(gone(&started).await);
}

/// An embedder about to exit (the service) ends every tool running now, one never cut short
/// included: its run ends, killed, and the tool is gone.
#[cfg(unix)]
#[tokio::test]
async fn end_all_now_ends_a_tool_still_running() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let (processes, job) = (SystemProcesses::default(), looping(&pid, "''"));
    let p = processes.clone();
    let run = tokio::spawn(async move { p.run(&job, &mut |_, _| {}).await });
    // Running once it wrote its pid and the processes hold it, however long a loaded host took.
    let mut tool = None;
    for _ in 0..15_000 {
        tool = tool.or_else(|| PidGuard::from_file(&pid));
        if tool.is_some() && !processes.running.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let tool = tool.expect("the tool started");
    processes.end_all_now();
    let exit = tokio::time::timeout(Duration::from_secs(30), run)
        .await
        .expect("ended at once, not at the end of its loop")
        .unwrap()
        .unwrap();
    assert_ne!(exit.code, Some(0), "killed, not finished");
    assert!(gone(&tool).await);
}
