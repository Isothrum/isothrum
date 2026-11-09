//! Real processes: each one a regression a runtime met.

use std::time::Duration;

use super::*;
use crate::process::capture;

fn spec(program: &str, args: &[&str]) -> ProcessSpec {
    ProcessSpec::new(program, args.iter().copied())
}

/// Whether process `pid` is gone within a few seconds.
async fn gone(pid: &str) -> bool {
    for _ in 0..60 {
        let alive = if cfg!(windows) {
            let out = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output();
            out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(pid))
        } else {
            // A zombie is gone too: in a container, nothing may reap the orphan it became.
            let out = std::process::Command::new("ps").args(["-o", "stat=", "-p", pid]).output();
            out.is_ok_and(|o| {
                let stat = String::from_utf8_lossy(&o.stdout);
                !stat.trim().is_empty() && !stat.trim().starts_with('Z')
            })
        };
        if !alive {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn a_missing_tool_is_named() {
    let err = capture(&SystemProcesses::default(), &spec("isothrum-no-such-tool", &[])).await.unwrap_err();
    assert!(
        matches!(err, ProcessError::Missing { ref program } if program == "isothrum-no-such-tool"),
        "{err:?}"
    );
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
    let limit = Duration::from_secs(if cfg!(windows) { 10 } else { 2 });
    assert!(tokio::time::timeout(limit, capture(&runner, &job)).await.is_err(), "cancelled while it ran");
    let pid = std::fs::read_to_string(&file).unwrap().trim().to_owned();
    assert!(gone(&pid).await, "the tool's child {pid} was left running");
}

#[cfg(unix)]
#[tokio::test]
async fn output_that_isnt_utf8_is_read_lossily_and_the_tool_runs_on() {
    // Regression: the first byte that wasn't UTF-8 failed the stream, which killed the tool.
    let script = r"printf 'Cr\351ation\r\n'; printf 'err \377\n' >&2; sleep 0.2; printf 'done'";
    let out = capture(&SystemProcesses::default(), &spec("sh", &["-c", script])).await.unwrap();
    assert_eq!(out.stdout, ["Cr\u{fffd}ation", "done"]);
    assert_eq!(out.stderr, ["err \u{fffd}"]);
    assert!(out.exit.is_some_and(Exit::success));
}

#[cfg(unix)]
#[tokio::test]
async fn a_tool_that_exited_is_not_waited_for_past_what_it_left_holding_its_output() {
    // Regression: a process the tool left behind (an ssh ControlMaster) held its pipes open, and
    // a finished `terraform destroy` hung the run, and its lock, for good.
    let started = std::time::Instant::now();
    let (runner, job) = (SystemProcesses::default(), spec("sh", &["-c", "sleep 20 & printf 'done'"]));
    let run = capture(&runner, &job);
    let out = tokio::time::timeout(Duration::from_secs(10), run).await.unwrap().unwrap();
    assert_eq!(out.stdout, ["done"]);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[cfg(unix)]
#[tokio::test]
async fn a_failure_keeps_its_exit_code_and_last_lines() {
    let out = capture(
        &SystemProcesses::default(),
        &spec("sh", &["-c", "for i in $(seq 1 12); do echo e$i >&2; done; exit 4"]),
    )
    .await
    .unwrap();
    assert_eq!(out.exit, Some(Exit { code: Some(4) }));
    assert_eq!(out.stderr.first().map(String::as_str), Some("e3"));
    assert_eq!(out.stderr.len(), 10);
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_comes_from_a_file_and_env_and_cwd_are_the_specs() {
    use crate::events::Stream;
    // Isoloom's exec check runners are piped into `docker compose exec -T … sh -s` this way.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("in.sh"), "echo \"$GREETING from $(basename \"$PWD\")\"\n").unwrap();
    let mut job = spec("sh", &["-s"]);
    job.stdin = Some(dir.path().join("in.sh"));
    job.env = vec![("GREETING".into(), "hello".into())];
    job.cwd = Some(dir.path().to_path_buf());
    let mut lines = Vec::new();
    let exit = SystemProcesses::default()
        .run(&job, &mut |s: Stream, l: &str| {
            lines.push((s, l.to_owned()));
        })
        .await
        .unwrap();
    let name = dir.path().file_name().unwrap().to_string_lossy().to_string();
    assert!(exit.success());
    assert_eq!(lines, [(Stream::Stdout, format!("hello from {name}"))]);
}

#[test]
fn only_the_windows_wrappers_go_through_cmd() {
    let az = spec("az", &["account", "show"]);
    let (program, args) = wrapped(&az);
    if cfg!(windows) {
        assert_eq!((program, args), ("cmd", vec!["/C", "az", "account", "show"]));
    } else {
        assert_eq!((program, args), ("az", vec!["account", "show"]));
    }
    assert_eq!(wrapped(&spec("docker", &["ps"])), ("docker", vec!["ps"]));
}

#[tokio::test]
async fn an_argument_cmd_would_interpret_is_refused_before_anything_starts() {
    let err = capture(&SystemProcesses::default(), &spec("az", &["group", "create", "--name", "x & calc"]))
        .await
        .unwrap_err();
    assert!(matches!(err, ProcessError::Refused { .. }), "{err:?}");
}
