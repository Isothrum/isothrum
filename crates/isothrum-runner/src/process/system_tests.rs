//! Real processes: each one a regression a runtime met.

use std::time::Duration;

use super::*;
use crate::process::capture;
#[cfg(unix)]
use crate::process::stopping::Stopping;

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

/// A `sh` job that writes its pid to `pid`, then loops until stopped; `on_int` is its SIGINT trap
/// (`''` to ignore it).
#[cfg(unix)]
fn looping(pid: &std::path::Path, on_int: &str) -> ProcessSpec {
    let script = format!("trap {on_int} INT; echo $$ > '{}'; while :; do sleep 0.1; done", pid.display());
    spec("sh", &["-c", &script])
}

/// The pid `file` holds, once the job wrote it.
#[cfg(unix)]
fn pid_in(file: &std::path::Path) -> String {
    std::fs::read_to_string(file).unwrap().trim().to_owned()
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
    assert!(tokio::time::timeout(Duration::from_secs(2), capture(&runner, &job)).await.is_err(), "cut short");
    tokio::time::timeout(Duration::from_secs(10), runner.stopped())
        .await
        .expect("it stopped on its own");
    assert!(!runner.is_stopping());
    assert_eq!(std::fs::read_to_string(&said).unwrap().trim(), "stopped", "its interrupt handler ran");
    assert!(gone(&pid_in(&pid)).await);
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
    assert!(
        tokio::time::timeout(Duration::from_secs(2), capture(&runner, &looping(&pid, "''")))
            .await
            .is_err()
    );
    assert!(runner.is_stopping(), "given its grace");
    tokio::time::timeout(Duration::from_secs(10), runner.stopped())
        .await
        .expect("ended after its grace");
    assert!(gone(&pid_in(&pid)).await);
}

/// A second Ctrl-C: the tools still stopping are ended at once.
#[cfg(unix)]
#[tokio::test]
async fn the_tools_still_stopping_can_be_ended_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("pid");
    let runner = SystemProcesses::default();
    assert!(
        tokio::time::timeout(Duration::from_secs(2), capture(&runner, &looping(&pid, "''")))
            .await
            .is_err()
    );
    runner.end_stopping();
    tokio::time::timeout(Duration::from_secs(5), runner.stopped())
        .await
        .expect("ended well before its 30 s");
    assert!(gone(&pid_in(&pid)).await);
}

#[cfg(unix)]
#[tokio::test]
async fn output_that_isnt_utf8_is_read_lossily_and_the_tool_runs_on() {
    // Regression: the first byte that wasn't UTF-8 failed the stream, which killed the tool.
    let script = r"printf 'Cr\351ation\r\n'; printf 'err \377\n' >&2; sleep 0.2; printf 'done'";
    let out = capture(&SystemProcesses::default(), &spec("sh", &["-c", script])).await.unwrap();
    assert_eq!(out.stdout, ["Cr\u{fffd}ation", "done"]);
    assert_eq!(out.stderr, ["err \u{fffd}"]);
    assert!(out.success());
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
    assert_eq!(out.exit, Exit { code: Some(4) });
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

#[test]
fn the_docker_cli_config_is_read_from_docker_config_before_the_home() {
    let (env_dir, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let context = |name: &str| format!(r#"{{"currentContext":"{name}"}}"#);
    std::fs::create_dir_all(home.path().join(".docker")).unwrap();
    std::fs::write(home.path().join(".docker/config.json"), context("colima")).unwrap();
    std::fs::write(env_dir.path().join("config.json"), context("default")).unwrap();
    let env = env_dir.path().display().to_string();
    let with = |docker_config: Option<&str>| {
        SystemProcesses::for_host(
            |v| (v == "DOCKER_CONFIG").then(|| docker_config.map(std::ffi::OsString::from)).flatten(),
            Some(home.path()),
        )
    };
    assert!(with(None).strip_engine_overrides, "the home's config picks a context");
    assert!(!with(Some(&env)).strip_engine_overrides, "$DOCKER_CONFIG's config comes first");
    assert!(!SystemProcesses::for_host(|_| None, None).strip_engine_overrides, "no config: nothing picked");
}

/// Regression: a `DOCKER_CONFIG` that isn't UTF-8 was read as unset, and the home's config,
/// another one, was read instead.
#[cfg(unix)]
#[test]
fn a_docker_config_that_isnt_utf8_is_still_the_one_read() {
    use std::os::unix::ffi::OsStringExt;
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".docker")).unwrap();
    std::fs::write(home.path().join(".docker/config.json"), r#"{"currentContext":"colima"}"#).unwrap();
    let elsewhere = std::ffi::OsString::from_vec(b"/nowhere/bad\xff".to_vec());
    let read = SystemProcesses::for_host(|v| (v == "DOCKER_CONFIG").then(|| elsewhere.clone()), Some(home.path()));
    assert!(!read.strip_engine_overrides, "the home's config isn't the one DOCKER_CONFIG names");
}

/// A Docker CLI config that is there but can't be read, or isn't JSON, read as "no context
/// picked" without a word: the inherited `DOCKER_HOST` stayed, and nothing said why.
#[tokio::test]
async fn a_docker_cli_config_that_cant_be_read_or_isnt_json_is_traced() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config.json");
    let docker_config = dir.path().as_os_str().to_owned();
    let read = || SystemProcesses::for_host(|v| (v == "DOCKER_CONFIG").then(|| docker_config.clone()), None);
    let (none, quiet) = crate::testing::traced(async { read() }).await;
    assert!(!none.strip_engine_overrides && quiet.is_empty(), "none there is fine: {quiet}");
    std::fs::create_dir(&config).unwrap();
    let (unreadable, traced) = crate::testing::traced(async { read() }).await;
    assert!(!unreadable.strip_engine_overrides);
    assert!(
        traced.contains("WARN") && traced.contains("config couldn't be read") && traced.contains("config.json"),
        "{traced}"
    );
    std::fs::remove_dir(&config).unwrap();
    std::fs::write(&config, r#"{"currentContext": "colima""#).unwrap();
    let (cut_short, not_json) = crate::testing::traced(async { read() }).await;
    assert!(!cut_short.strip_engine_overrides);
    assert!(not_json.contains("WARN") && not_json.contains("config isn't JSON"), "{not_json}");
}

/// A tool run with a folder to record it in is there while it runs (it lists its own entry,
/// named by its process id), and gone once it exited.
#[cfg(unix)]
#[tokio::test]
async fn a_recorded_tool_is_in_the_runs_tools_while_it_runs_and_gone_after() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(crate::process::ledger::DIR);
    // The entry is written once the tool started: it waits for it (5 s at most), then lists.
    let script = "echo \"$$\"; i=0; while [ $i -lt 50 ] && ! ls \"$0\" | grep -q \"-$$.tool\"; do sleep 0.1; i=$((i+1)); done; ls \"$0\"";
    let mut listing = spec("sh", &["-c", script, dir.to_str().unwrap()]);
    listing.tools = Some(dir.clone());
    let out = capture(&SystemProcesses::default(), &listing).await.unwrap();
    assert!(out.success(), "{out:?}");
    let pid = &out.stdout[0];
    assert!(
        out.stdout.iter().any(|l| l.ends_with(&format!("-{pid}.tool"))),
        "the tool's own entry, named by its id: {:?}",
        out.stdout
    );
    let tools: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "tool"))
        .collect();
    assert!(tools.is_empty(), "{tools:?}");
    // Unrecorded: nothing is written.
    let other = tempfile::tempdir().unwrap();
    capture(&SystemProcesses::default(), &spec("true", &[])).await.unwrap();
    assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
}
