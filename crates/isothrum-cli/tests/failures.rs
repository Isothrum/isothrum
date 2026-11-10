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
    let (home, run_dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let run_dir = run_dir.path().to_str().unwrap();
    let out = isothrum(home.path(), &["up", run_dir, "--name", "ci-run", "--json"]).unwrap();
    assert_eq!(out.status.code(), Some(2));
    let event = last_event(&out).unwrap();
    assert_eq!((event["event"].as_str(), event["code"].as_str()), (Some("error"), Some("usage")), "{event}");
    assert_eq!(
        (event["op"].as_str(), event["run"].as_str(), event["seq"].as_u64()),
        (Some("up"), Some("ci-run"), Some(1))
    );
    assert!(event["message"].as_str().unwrap().contains("isoloom generate"), "{event}");
    assert!(out.stderr.is_empty(), "nothing else: {}", String::from_utf8_lossy(&out.stderr));

    let text = isothrum(home.path(), &["status", run_dir]).unwrap();
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

#[cfg(unix)]
#[test]
fn a_value_that_isnt_utf8_is_a_usage_error_event_with_json() {
    // Regression: telling a refused command line asked for JSON read the arguments as UTF-8, and
    // panicked (exit 101) on one that isn't.
    use std::os::unix::ffi::OsStrExt;

    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let out = Command::new(env!("CARGO_BIN_EXE_isothrum"))
        .arg("up")
        .arg(run.path())
        .arg("--name")
        .arg(std::ffi::OsStr::from_bytes(b"r\xffn"))
        .args(["--json", "--home"])
        .arg(home.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let event = last_event(&out).unwrap();
    assert_eq!((event["event"].as_str(), event["code"].as_str()), (Some("error"), Some("usage")), "{event}");
}

/// A tiny `docker` for `web-portal`: every call answers at once, except the start, which writes
/// its process id to `$ISOTHRUM_TEST_PIDFILE` and then runs until it is stopped. On SIGINT it
/// runs `$ISOTHRUM_TEST_ON_INT` (default: exit; empty: SIGINT is ignored).
#[cfg(unix)]
const FAKE_DOCKER: &str = r#"#!/bin/sh
case " $* " in
  *" compose version "*) echo 2.29.0 ;;
  *" config --format json "*) echo '{"services":{"web":{},"db":{}}}' ;;
  *" up --detach "*)
    trap "${ISOTHRUM_TEST_ON_INT-exit 130}" INT
    echo $$ > "$ISOTHRUM_TEST_PIDFILE"; echo starting
    while :; do sleep 0.1; done ;;
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
    let up = Up::start(None).unwrap();
    up.signal("-INT").unwrap();
    up.ended().unwrap().assert_cancelled(130);
}

#[cfg(unix)]
#[test]
fn sigterm_stops_the_tools_ends_the_stream_with_cancelled_and_exits_143() {
    // A CI runner cancelling its job sends SIGTERM.
    let up = Up::start(None).unwrap();
    up.signal("-TERM").unwrap();
    up.ended().unwrap().assert_cancelled(143);
}

/// A tool is asked to stop (SIGINT, as Terraform needs to save its state), and the command
/// exits only once it did.
#[cfg(unix)]
#[test]
fn a_cancelled_command_waits_for_its_tools_to_stop() {
    let up = Up::start(Some(r#"sleep 1; echo stopped > "$ISOTHRUM_TEST_STOPPED"; exit 0"#)).unwrap();
    up.signal("-INT").unwrap();
    let ended = up.ended().unwrap();
    ended.assert_cancelled(130);
    assert_eq!(ended.on_int_wrote.trim(), "stopped", "{}", ended.stderr);
    assert!(ended.stderr.contains("Waiting for the tools to stop"), "{}", ended.stderr);
}

/// A second signal ends the tools still stopping at once, rather than after their 30 s.
#[cfg(unix)]
#[test]
fn a_second_signal_ends_the_tools_at_once() {
    let up = Up::start(Some("")).unwrap();
    let started = std::time::Instant::now();
    up.signal("-INT").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    up.signal("-INT").unwrap();
    up.ended().unwrap().assert_cancelled(130);
    assert!(started.elapsed() < std::time::Duration::from_secs(15), "{:?}", started.elapsed());
}

/// What a command sent a signal showed.
#[cfg(unix)]
struct Signalled {
    /// Its exit code.
    code: Option<i32>,
    /// Its standard error.
    stderr: String,
    /// Its last event.
    event: Value,
    /// Whether the tool it started was gone after it.
    tool_stopped: bool,
    /// What the tool's SIGINT handler wrote by the time the command ended.
    on_int_wrote: String,
}

#[cfg(unix)]
impl Signalled {
    /// The command exited `code`, its stream ended with a `cancelled` error, and its tool is gone.
    fn assert_cancelled(&self, code: i32) {
        assert_eq!(self.code, Some(code), "{}", self.stderr);
        let field = |name: &str| self.event.get(name).and_then(Value::as_str);
        let last = (field("event"), field("code"), field("op"));
        assert_eq!(last, (Some("error"), Some("cancelled"), Some("up")), "{}", self.event);
        assert!(self.tool_stopped, "the tool outlived the command");
    }
}

/// `isothrum up --json` on a tool that runs until it is stopped, once the tool runs.
#[cfg(unix)]
struct Up {
    /// The command.
    child: std::process::Child,
    /// The tool's process id.
    tool: String,
    /// Where the tool's SIGINT handler may write.
    stopped: std::path::PathBuf,
    /// The home, the run's folder and the fake tool's, kept while the command runs.
    _dirs: [tempfile::TempDir; 3],
}

#[cfg(unix)]
impl Up {
    /// Starts it, the tool running `on_int` on SIGINT (`None`: it exits).
    fn start(on_int: Option<&str>) -> Outcome<Up> {
        use std::os::unix::fs::PermissionsExt;

        let (home, run_dir, bin) = (tempfile::tempdir()?, tempfile::tempdir()?, tempfile::tempdir()?);
        let dir = run_dir.path().join(".isoloom/docker");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("run.json"), include_str!("../../isothrum-runner/tests/fixtures/web-portal.run.json"))?;
        let docker = bin.path().join("docker");
        std::fs::write(&docker, FAKE_DOCKER)?;
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755))?;
        let (pidfile, stopped) = (bin.path().join("docker.pid"), bin.path().join("stopped"));
        let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_isothrum"));
        cmd.args(["up", "--json", "--secret-from-env", "DEMO_SECRET", "--home"])
            .arg(home.path())
            .arg(run_dir.path())
            .env("PATH", path)
            .env("DEMO_SECRET", "token-x")
            .env("ISOTHRUM_TEST_PIDFILE", &pidfile)
            .env("ISOTHRUM_TEST_STOPPED", &stopped)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        if let Some(on_int) = on_int {
            cmd.env("ISOTHRUM_TEST_ON_INT", on_int);
        }
        let child = cmd.spawn()?;
        if !wait_until(|| std::fs::read_to_string(&pidfile).is_ok_and(|p| !p.trim().is_empty())) {
            return Err("the start never began".into());
        }
        let tool = std::fs::read_to_string(&pidfile)?.trim().to_owned();
        if !alive(&tool) {
            return Err(format!("the tool {tool} wasn't running").into());
        }
        Ok(Up {
            child,
            tool,
            stopped,
            _dirs: [home, run_dir, bin],
        })
    }

    /// Sends the command `signal`, as `kill` names it.
    fn signal(&self, signal: &str) -> Outcome<()> {
        if Command::new("kill").args([signal, &self.child.id().to_string()]).status()?.success() {
            Ok(())
        } else {
            Err(format!("kill {signal} failed").into())
        }
    }

    /// What the command showed once it ended.
    fn ended(self) -> Outcome<Signalled> {
        let out = self.child.wait_with_output()?;
        let on_int_wrote = std::fs::read_to_string(&self.stopped).unwrap_or_default();
        let tool = self.tool;
        Ok(Signalled {
            code: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            event: last_event(&out)?,
            tool_stopped: wait_until(|| !alive(&tool)),
            on_int_wrote,
        })
    }
}

/// Regression: an `ISOTHRUM_HOME` that isn't UTF-8 was read as unset, and the runner went to
/// `~/.isothrum`.
#[cfg(unix)]
#[test]
fn a_home_variable_that_isnt_utf8_is_never_passed_over_for_the_users() {
    use std::os::unix::ffi::OsStringExt;
    let dir = tempfile::tempdir().unwrap();
    let mut home = dir.path().as_os_str().to_owned().into_vec();
    home.extend_from_slice(b"/bad\xff");
    let user = dir.path().join("user");
    std::fs::create_dir(&user).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_isothrum"))
        .arg("key")
        .env("ISOTHRUM_HOME", std::ffi::OsString::from_vec(home))
        .env("HOME", &user)
        .output()
        .unwrap();
    assert!(!user.join(".isothrum").exists(), "{}", String::from_utf8_lossy(&out.stderr));
}
