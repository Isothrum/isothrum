//! How the `isothrum` binary fails, as a CI job sees it: its exit code, and with `--json` an
//! `error` event on standard output for every failure, one before any operation began included,
//! and the tools of a cancelled command stopped with it.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// A helper's outcome: a failed step is the test's report.
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// `isothrum <args>` with `home` as its home.
fn isothrum(home: &Path, args: &[&str]) -> Outcome<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_isothrum")).args(args).arg("--home").arg(home).output()?)
}

/// The last line of standard output, as JSON.
fn last_event(out: &Output) -> Outcome<Value> {
    let stdout = String::from_utf8_lossy(&out.stdout);
    let last = stdout
        .lines()
        .last()
        .ok_or_else(|| format!("no output; standard error: {}", String::from_utf8_lossy(&out.stderr)))?;
    Ok(serde_json::from_str(last)?)
}

#[test]
fn a_folder_without_a_manifest_is_a_usage_error_event_with_json() {
    let (home, lab) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let lab = lab.path().to_str().unwrap();
    let out = isothrum(home.path(), &["up", lab, "--name", "ci-run", "--json"]).unwrap();
    assert_eq!(out.status.code(), Some(2));
    let event = last_event(&out).unwrap();
    assert_eq!((event["event"].as_str(), event["code"].as_str()), (Some("error"), Some("usage")), "{event}");
    assert_eq!(
        (event["op"].as_str(), event["run"].as_str(), event["seq"].as_u64()),
        (Some("up"), Some("ci-run"), Some(1))
    );
    assert!(event["message"].as_str().unwrap().contains("isoloom generate"), "{event}");
    assert!(out.stderr.is_empty(), "nothing else: {}", String::from_utf8_lossy(&out.stderr));

    let text = isothrum(home.path(), &["status", lab]).unwrap();
    assert_eq!(text.status.code(), Some(2));
    assert!(text.stdout.is_empty() && String::from_utf8_lossy(&text.stderr).starts_with("error: "));
}

#[test]
fn a_command_line_clap_refuses_is_a_usage_error_event_with_json() {
    let home = tempfile::tempdir().unwrap();
    let out = isothrum(home.path(), &["up", "--no-such-flag", "--json"]).unwrap();
    assert_eq!(out.status.code(), Some(2));
    let event = last_event(&out).unwrap();
    assert_eq!(
        (event["code"].as_str(), event.get("op"), event.get("run")),
        (Some("usage"), None, None),
        "{event}"
    );
    assert!(event["message"].as_str().unwrap().contains("--no-such-flag"));
}

/// A tiny `docker` for `flag-portal`: every call answers at once, except the start, which writes
/// its process id to `$ISOTHRUM_TEST_PIDFILE` and then runs (as `sleep`) until it is stopped.
#[cfg(unix)]
const FAKE_DOCKER: &str = r#"#!/bin/sh
case " $* " in
  *" compose version "*) echo 2.29.0 ;;
  *" config --format json "*) echo '{"services":{"web":{},"db":{}}}' ;;
  *" up --detach "*) echo $$ > "$ISOTHRUM_TEST_PIDFILE"; echo starting; exec sleep 300 ;;
esac
exit 0
"#;

/// Whether process `pid` is still there.
#[cfg(unix)]
fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Waits up to 20 s for `ready`.
#[cfg(unix)]
fn wait_until(ready: impl Fn() -> bool) -> bool {
    for _ in 0..200 {
        if ready() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}

#[cfg(unix)]
#[test]
fn sigint_stops_the_tools_ends_the_stream_with_cancelled_and_exits_130() {
    use std::os::unix::fs::PermissionsExt;

    let (home, lab, bin) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = lab.path().join(".isoloom/docker");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.json"), include_str!("../../isothrum-runner/tests/fixtures/flag-portal.run.json")).unwrap();
    let docker = bin.path().join("docker");
    std::fs::write(&docker, FAKE_DOCKER).unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pidfile = bin.path().join("docker.pid");
    let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());

    let child = Command::new(env!("CARGO_BIN_EXE_isothrum"))
        .args(["up", "--json", "--secret-from-env", "CTF_FLAG_MAIN", "--home"])
        .arg(home.path())
        .arg(lab.path())
        .env("PATH", path)
        .env("CTF_FLAG_MAIN", "flag{x}")
        .env("ISOTHRUM_TEST_PIDFILE", &pidfile)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    assert!(
        wait_until(|| std::fs::read_to_string(&pidfile).is_ok_and(|p| !p.trim().is_empty())),
        "the start never began"
    );
    let tool = std::fs::read_to_string(&pidfile).unwrap().trim().to_owned();
    assert!(alive(&tool));

    let sent = Command::new("kill").args(["-INT", &child.id().to_string()]).status().unwrap();
    assert!(sent.success());
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(130), "{}", String::from_utf8_lossy(&out.stderr));
    let event = last_event(&out).unwrap();
    assert_eq!(
        (event["event"].as_str(), event["code"].as_str(), event["op"].as_str()),
        (Some("error"), Some("cancelled"), Some("up")),
        "{event}"
    );
    assert!(wait_until(|| !alive(&tool)), "the tool {tool} outlived the command");
}
