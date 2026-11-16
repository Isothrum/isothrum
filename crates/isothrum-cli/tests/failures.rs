//! How the `isothrum` binary fails, as a CI job sees it: its exit code, and with `--json` an
//! `error` event on standard output for every failure, one before any operation began included,
//! and the tools of a cancelled command stopped with it.
#![expect(
    clippy::disallowed_methods,
    reason = "a test starts the binary and the programs it checks against: it is no adapter of the product"
)]

mod common;

use common::{Lab, last_event};
#[cfg(unix)]
use common::{Outcome, json_lines};
#[cfg(unix)]
use isothrum_test_support::{ChildGuard, PidGuard, alive, read_pid};
#[cfg(unix)]
use serde_json::Value;

#[test]
fn a_folder_that_holds_no_project_is_a_project_invalid_error_event_with_json() {
    let lab = Lab::empty().unwrap();
    let out = lab.isothrum(&["up", lab.dir(), "--name", "ci-run", "--json"]).unwrap();
    assert_eq!(out.status.code(), Some(1));
    let event = last_event(&out).unwrap();
    assert_eq!(
        (event["event"].as_str(), event["code"].as_str()),
        (Some("error"), Some("project_invalid")),
        "{event}"
    );
    assert_eq!(
        (event["op"].as_str(), event["run"].as_str(), event["seq"].as_u64()),
        (Some("up"), Some("ci-run"), Some(1))
    );
    assert!(out.stderr.is_empty(), "nothing else: {}", String::from_utf8_lossy(&out.stderr));

    let text = lab.isothrum(&["status", lab.dir()]).unwrap();
    assert_eq!(text.status.code(), Some(1));
    assert!(text.stdout.is_empty() && String::from_utf8_lossy(&text.stderr).starts_with("error: "));
}

#[test]
fn a_command_line_clap_refuses_is_a_usage_error_event_with_json() {
    let lab = Lab::empty().unwrap();
    let out = lab.isothrum(&["up", "--no-such-flag", "--json"]).unwrap();
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

    let lab = Lab::empty().unwrap();
    let out = lab
        .command()
        .unwrap()
        .args(["up", lab.dir(), "--name"])
        .arg(std::ffi::OsStr::from_bytes(b"r\xffn"))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    let event = last_event(&out).unwrap();
    assert_eq!((event["event"].as_str(), event["code"].as_str()), (Some("error"), Some("usage")), "{event}");
}

/// How long a test waits for what a command does on its own: long, as it only bounds a failure,
/// and a loaded machine starts a process slowly.
#[cfg(unix)]
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(90);

/// Waits for `ready`, [`PATIENCE`] at most.
#[cfg(unix)]
fn wait_until(ready: impl FnMut() -> bool) -> bool {
    isothrum_test_support::wait_until(PATIENCE, ready)
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
/// exits only once it did: the tool takes a second to stop, and has said so by then.
#[cfg(unix)]
#[test]
fn a_cancelled_command_waits_for_its_tools_to_stop() {
    let up = Up::start(Some(r#"sleep 1; echo stopped > "$ISOTHRUM_TEST_DOCKER/stopped"; exit 0"#)).unwrap();
    up.signal("-INT").unwrap();
    let ended = up.ended().unwrap();
    ended.assert_cancelled(130);
    assert_eq!(ended.on_int_wrote.trim(), "stopped", "{}", ended.stderr);
    assert!(ended.stderr.contains("Waiting for the tools to stop"), "{}", ended.stderr);
}

/// A second signal ends the tools still stopping at once, rather than after their 30 s grace.
#[cfg(unix)]
#[test]
fn a_second_signal_ends_the_tools_at_once() {
    // The tool ignores the request to stop.
    let up = Up::start(Some("")).unwrap();
    up.signal("-INT").unwrap();
    let first = std::time::Instant::now();
    // The second signal goes once the command says it waits for the tools: sent as soon as the
    // tool had the first, two signals that close read as one, and the tools were waited for
    // their whole grace.
    assert!(wait_until(|| up.said("Waiting for the tools to stop")), "the command took the first signal");
    up.signal("-INT").unwrap();
    up.ended().unwrap().assert_cancelled(130);
    // The grace runs from the first signal: a command that ended before it was cut short.
    let grace = std::time::Duration::from_secs(30);
    assert!(first.elapsed() < grace, "the tools were waited for: {:?}", first.elapsed());
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

/// `isothrum up --json` on a stand-in `docker` whose start runs until it is stopped, once the
/// start runs. The command and the tool are killed when it is dropped, whatever way the test
/// ended.
#[cfg(unix)]
struct Up {
    /// The command.
    child: ChildGuard,
    /// The tool.
    tool: PidGuard,
    /// Its folders, kept while the command runs.
    lab: Lab,
    /// What the command printed on standard error so far, read as it prints it.
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
    /// The thread reading it, done once the command closed it.
    reading: std::thread::JoinHandle<()>,
}

#[cfg(unix)]
impl Up {
    /// Starts it, the tool running `on_int` on SIGINT (`None`: it exits).
    fn start(on_int: Option<&str>) -> Outcome<Up> {
        let lab = Lab::compose()?;
        let pidfile = lab.docker_file("docker.pid");
        let mut cmd = lab.command()?;
        cmd.args(["up", "--json", "--secret-from-env", "DEMO_SECRET", lab.dir()])
            .env("DEMO_SECRET", "token-x")
            .env("ISOTHRUM_TEST_PIDFILE", &pidfile)
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
            return Err(format!("the start never began in {PATIENCE:?}").into());
        }
        let tool = PidGuard::from_file(&pidfile).ok_or("the tool's pid")?;
        if !alive(tool.id()) {
            return Err(format!("the tool {} wasn't running", tool.id()).into());
        }
        Ok(Up {
            child,
            tool,
            lab,
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
        if std::process::Command::new("kill")
            .args([signal, &self.child.id().to_string()])
            .status()?
            .success()
        {
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
        let on_int_wrote = std::fs::read_to_string(self.lab.docker_file("stopped")).unwrap_or_default();
        let events = json_lines(&out)?;
        let tool = self.tool;
        Ok(Signalled {
            code: out.status.code(),
            stderr,
            event: events.last().cloned().ok_or("no event")?,
            tool_stopped: wait_until(|| !alive(tool.id())),
            on_int_wrote,
        })
    }
}

/// Regression: an `ISOTHRUM_HOME` that isn't UTF-8 was read as unset, and the runner went to
/// `~/.isothrum`.
#[cfg(unix)]
#[test]
fn a_home_variable_that_isnt_utf8_is_the_home_never_the_users() {
    use std::os::unix::ffi::OsStringExt;
    let lab = Lab::empty().unwrap();
    let mut home = lab.home.path().as_os_str().to_owned().into_vec();
    home.extend_from_slice(b"/bad\xff");
    let home = std::path::PathBuf::from(std::ffi::OsString::from_vec(home));
    // A file system that refuses the name (APFS) makes it an I/O error naming it.
    let accepted = std::fs::create_dir(&home).is_ok();
    let out = lab.command().unwrap().args(["reap", "--json"]).env("ISOTHRUM_HOME", &home).output().unwrap();
    let said = last_event(&out).unwrap();
    if accepted {
        assert_eq!((out.status.code(), said["event"].as_str()), (Some(0), Some("result")), "{said}");
        assert!(home.join("state.db").is_file(), "the runner's home is the variable's");
    } else {
        assert_eq!((out.status.code(), said["code"].as_str()), (Some(1), Some("io")), "{said}");
        let message = said["message"].as_str().unwrap_or_default();
        assert!(message.contains(lab.home.path().to_str().unwrap()), "the home named is the variable's: {said}");
    }
    assert!(!lab.env.path().join(".isothrum").exists(), "never the user's");
}

/// Regression: an `ISOTHRUM_LOG` that isn't UTF-8 was read as unset, and the tracing asked for
/// was left off without a word.
#[cfg(unix)]
#[test]
fn a_log_variable_that_isnt_utf8_is_warned_about() {
    use std::os::unix::ffi::OsStringExt;
    let lab = Lab::empty().unwrap();
    let out = lab
        .command()
        .unwrap()
        .args(["completions", "bash"])
        .env("ISOTHRUM_LOG", std::ffi::OsString::from_vec(b"bad\xff".to_vec()))
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && said.contains("warning: ISOTHRUM_LOG isn't UTF-8"), "{said}");
}
