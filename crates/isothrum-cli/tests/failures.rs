//! How the `isothrum` binary fails, as a CI job sees it: its exit code, and with `--json` an
//! `error` event on standard output for every failure, one before any operation began included,
//! and the tools of a cancelled command stopped with it.
#![expect(
    clippy::disallowed_methods,
    reason = "a test starts the binary and the programs it checks against: it is no adapter of the product"
)]

use std::path::Path;
use std::process::{Command, Output};

#[cfg(unix)]
use isothrum_test_support::{ChildGuard, PidGuard, TempHome, alive, read_pid, write_executable};
use serde_json::Value;

/// A helper's outcome: a failed step is the test's report.
type Outcome<T> = Result<T, Box<dyn std::error::Error>>;

/// `isothrum <args>` with `home` as its home.
fn isothrum(home: &Path, args: &[&str]) -> Outcome<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_isothrum"))
        .env("ISOTHRUM_NO_SERVICE", "1")
        .args(args)
        .arg("--home")
        .arg(home)
        .output()?)
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
        .env("ISOTHRUM_NO_SERVICE", "1")
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
/// its process id to `$ISOTHRUM_TEST_PIDFILE` and then runs until it is stopped (two minutes at
/// most, should the test binary be killed before its guards ran). On SIGINT it runs
/// `$ISOTHRUM_TEST_ON_INT` (default: exit; empty: SIGINT is ignored).
#[cfg(unix)]
const FAKE_DOCKER: &str = r#"#!/bin/sh
case " $* " in
  *" info --format "*) echo 29.0.0 ;;
  *" compose version "*) echo 2.29.0 ;;
  *" config --format json "*) echo '{"services":{"web":{},"db":{}}}' ;;
  *" up --detach "*)
    trap "${ISOTHRUM_TEST_ON_INT-exit 130}" INT
    echo $$ > "$ISOTHRUM_TEST_PIDFILE"; echo starting
    i=0; while [ $i -lt 1200 ]; do sleep 0.1; i=$((i+1)); done ;;
esac
exit 0
"#;

/// Waits up to 20 s for `ready`.
#[cfg(unix)]
fn wait_until(ready: impl FnMut() -> bool) -> bool {
    isothrum_test_support::wait_until(std::time::Duration::from_secs(20), ready)
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
    // The tool ignores the request to stop.
    let up = Up::start(Some("")).unwrap();
    let started = std::time::Instant::now();
    up.signal("-INT").unwrap();
    // The second signal goes once the command says it waits for the tools: sent as soon as the
    // tool had the first, two signals that close read as one, and the tools were waited for
    // their whole 30 s.
    assert!(wait_until(|| up.said("Waiting for the tools to stop")), "the command took the first signal");
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

/// `isothrum up --json` on a tool that runs until it is stopped, once the tool runs. The command
/// and the tool are killed when it is dropped, whatever way the test ended.
#[cfg(unix)]
struct Up {
    /// The command.
    child: ChildGuard,
    /// The tool.
    tool: PidGuard,
    /// Where the tool's SIGINT handler may write.
    stopped: std::path::PathBuf,
    /// The home, the run's folder and the fake tool's, kept while the command runs.
    _dirs: [tempfile::TempDir; 3],
    /// The command's home folder, environment and locale (not the user's).
    _env: TempHome,
    /// What the command printed on standard error so far, read as it prints it.
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    /// The thread reading it, done once the command closed it.
    reading: std::thread::JoinHandle<()>,
}

#[cfg(unix)]
impl Up {
    /// Starts it, the tool running `on_int` on SIGINT (`None`: it exits).
    fn start(on_int: Option<&str>) -> Outcome<Up> {
        let (home, run_dir, bin) = (tempfile::tempdir()?, tempfile::tempdir()?, tempfile::tempdir()?);
        let dir = run_dir.path().join(".isoloom/docker");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("run.json"), include_str!("../../isothrum-runner/tests/fixtures/web-portal.run.json"))?;
        // Written by a child process: one this test wrote itself could be busy ("Text file busy")
        // in a process another test forked meanwhile, when the command runs it.
        write_executable(&bin.path().join("docker"), FAKE_DOCKER)?;
        let (pidfile, stopped) = (bin.path().join("docker.pid"), bin.path().join("stopped"));
        let path = format!("{}:{}", bin.path().display(), std::env::var("PATH").unwrap_or_default());

        let env = TempHome::new();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_isothrum"));
        cmd.envs(env.envs());
        cmd.env("ISOTHRUM_NO_SERVICE", "1");
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
        let mut child = ChildGuard::spawn(&mut cmd)?;
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let reading = {
            let (pipe, said) = (child.take_stderr().ok_or("the command's stderr")?, std::sync::Arc::clone(&stderr));
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(pipe).lines().map_while(Result::ok) {
                    let mut said = said.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    said.push_str(&line);
                    said.push('\n');
                }
            })
        };
        if !wait_until(|| read_pid(&pidfile).is_some()) {
            return Err("the start never began".into());
        }
        let tool = PidGuard::from_file(&pidfile).ok_or("the tool's pid")?;
        if !alive(tool.id()) {
            return Err(format!("the tool {} wasn't running", tool.id()).into());
        }
        Ok(Up {
            child,
            tool,
            stopped,
            _dirs: [home, run_dir, bin],
            _env: env,
            stderr,
            reading,
        })
    }

    /// Whether the command printed `text` on standard error yet.
    fn said(&self, text: &str) -> bool {
        self.stderr.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains(text)
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
        self.reading.join().map_err(|_| "the stderr reader panicked")?;
        let stderr = self.stderr.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let on_int_wrote = std::fs::read_to_string(&self.stopped).unwrap_or_default();
        let tool = self.tool;
        Ok(Signalled {
            code: out.status.code(),
            stderr,
            event: last_event(&out)?,
            tool_stopped: wait_until(|| !alive(tool.id())),
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
        .env("ISOTHRUM_NO_SERVICE", "1")
        .arg("key")
        .env("ISOTHRUM_HOME", std::ffi::OsString::from_vec(home))
        .env("HOME", &user)
        .output()
        .unwrap();
    assert!(!user.join(".isothrum").exists(), "{}", String::from_utf8_lossy(&out.stderr));
}

/// Regression: an `ISOTHRUM_LOG` that isn't UTF-8 was read as unset, and the tracing asked for
/// was left off without a word.
#[cfg(unix)]
#[test]
fn a_log_variable_that_isnt_utf8_is_warned_about() {
    use std::os::unix::ffi::OsStringExt;
    let out = Command::new(env!("CARGO_BIN_EXE_isothrum"))
        .env("ISOTHRUM_NO_SERVICE", "1")
        .args(["completions", "bash"])
        .env("ISOTHRUM_LOG", std::ffi::OsString::from_vec(b"bad\xff".to_vec()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.contains("warning: ISOTHRUM_LOG isn't UTF-8"), "{said}");
}
