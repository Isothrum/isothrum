//! The `ready_file` condition: a run host's bootstrap writes its progress to a file in the
//! `isoloom-ready/1` grammar (`running: <step>`, `failed: <step>`, `ready`; no file yet: still
//! booting), read over SSH every `interval_s` with the bootstrap log's tail. A host that never
//! answers is a failure after `unreachable_timeout_s`, never a success.

use std::time::Duration;

use crate::Result;
use crate::error::Error;
use crate::events::ReadyState;
use crate::executor::{Cx, Echo, call};
use crate::process::Captured;
use crate::ssh::{self, Login};

/// How long a host may stay unreachable when the manifest says nothing.
const UNREACHABLE_DEFAULT: Duration = Duration::from_secs(600);

/// One read (the file's first line, the log's tail) gives up after this long, so the deadlines
/// hold whatever SSH does.
const READ_LIMIT: Duration = Duration::from_secs(60);

/// How many lines of the bootstrap's log explain a failure.
const LOG_TAIL: usize = 25;

/// What separates the file's line from the log's tail in one read's output.
const LOG_MARK: &str = "--isoloom-log--";

/// The bootstrap's progress, from the ready file's first line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Progress {
    /// No file yet, or nothing the grammar knows.
    Booting,
    /// A step runs.
    Running(String),
    /// Done.
    Ready,
    /// A step failed; nothing after it runs.
    Failed(String),
}

/// The ready file's first line, read with `isoloom-ready/1`.
pub(crate) fn progress(line: &str) -> Progress {
    let line = line.trim();
    if line == "ready" {
        Progress::Ready
    } else if let Some(step) = line.strip_prefix("failed:") {
        Progress::Failed(step.trim().to_owned())
    } else if let Some(step) = line.strip_prefix("running:") {
        Progress::Running(step.trim().to_owned())
    } else {
        Progress::Booting
    }
}

/// What a `ready_file` condition waits for.
#[derive(Debug, Clone)]
pub(crate) struct Wait {
    /// The condition, as events name it (`ready_file 10.60.0.10`).
    pub(crate) condition: String,
    /// Where the file is.
    pub(crate) login: Login,
    /// The file.
    pub(crate) path: String,
    /// The bootstrap's log.
    pub(crate) log: Option<String>,
    /// How long the bootstrap may take.
    pub(crate) within: Duration,
    /// How long the host may stay unreachable.
    pub(crate) unreachable: Option<Duration>,
    /// Between two reads.
    pub(crate) every: Duration,
}

/// The remote command of one read: the file's first line, a mark, the log's tail; it succeeds
/// whatever is missing, so only an SSH failure fails it.
fn read_command(path: &str, log: Option<&str>) -> String {
    let tail = log.map_or(String::new(), |l| format!("tail -n {LOG_TAIL} {} 2>/dev/null; ", ssh::sh_quote(l)));
    format!("head -n 1 {} 2>/dev/null; echo {LOG_MARK}; {tail}true", ssh::sh_quote(path))
}

/// A read's output as the file's first line and the log's non-empty lines.
fn split(out: &[String]) -> (String, Vec<String>) {
    let mark = out.iter().position(|l| l == LOG_MARK).unwrap_or(out.len());
    let first = out.get(..mark).and_then(|f| f.first()).cloned().unwrap_or_default();
    let tail = out
        .get(mark + 1..)
        .unwrap_or_default()
        .iter()
        .filter(|l| !l.trim().is_empty())
        .cloned()
        .collect();
    (first, tail)
}

/// Waits until the file says `ready`; fails when it says `failed:` (with the log's tail), when
/// the bootstrap runs out of time, or when the host never answered. A read that fails is said
/// (a `ready` event, `waiting`, each time its reason changes), and the last one's reason ends
/// the error a wait that runs out of time fails with.
pub(crate) async fn wait(cx: &Cx<'_>, w: &Wait) -> Result<()> {
    let spec = ssh::spec(cx, &w.login, &read_command(&w.path, w.log.as_deref()), Vec::new())?;
    let started = tokio::time::Instant::now();
    let unreachable = w.unreachable.unwrap_or(UNREACHABLE_DEFAULT);
    let (mut reached, mut last, mut failed) = (false, Progress::Booting, None::<String>);
    loop {
        match call(cx, &w.condition, &spec, READ_LIMIT, Echo::Quiet).await {
            Ok(out) if out.succeeded() => {
                (reached, failed) = (true, None);
                let (line, tail) = split(&out.stdout);
                if let Some(done) = settle(cx, w, progress(&line), &tail, &mut last) {
                    return done;
                }
            }
            Err(e @ Error::ToolMissing { .. }) => return Err(e),
            read => {
                let why = cx.events.mask(&read.map_or_else(|e| e.to_string(), |out| read_failure(&out)));
                if failed.as_deref() != Some(why.as_str()) {
                    cx.events.ready(&w.condition, ReadyState::Waiting, &format!("no answer over SSH yet: {why}"));
                }
                if !reached && started.elapsed() >= unreachable {
                    let detail = format!("{} didn't answer over SSH within {}s: {why}", w.login.target, unreachable.as_secs());
                    return Err(not_ready(cx, w, detail));
                }
                failed = Some(why);
            }
        }
        if started.elapsed() >= w.within {
            return Err(not_ready(cx, w, out_of_time(w, &last, failed.as_deref())));
        }
        tokio::time::sleep(w.every).await;
    }
}

/// Why a read that ran failed: ssh's last error line (else its output's), with its exit status.
fn read_failure(out: &Captured) -> String {
    let said = |lines: &[String]| lines.iter().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_owned());
    let status = out.exit.code.map_or_else(|| "a signal".to_owned(), |c| format!("code {c}"));
    match said(&out.stderr).or_else(|| said(&out.stdout)) {
        Some(line) => format!("{line} (exited with {status})"),
        None => format!("exited with {status}"),
    }
}

/// What a wait out of time says: the last step the file named, and why the last read failed
/// when it did.
fn out_of_time(w: &Wait, last: &Progress, failed: Option<&str>) -> String {
    let step = match last {
        Progress::Running(s) => s.as_str(),
        _ => "booting",
    };
    let detail = format!("not done within {}s (last step: {step})", w.within.as_secs());
    match failed {
        Some(why) => format!("{detail}; its last read over SSH failed: {why}"),
        None => detail,
    }
}

/// What one answer means: the end of the wait (ready, or failed with the log's tail), or none
/// while it goes on (a new step is an event).
fn settle(cx: &Cx<'_>, w: &Wait, now: Progress, tail: &[String], last: &mut Progress) -> Option<Result<()>> {
    match &now {
        Progress::Ready => {
            cx.events.ready(&w.condition, ReadyState::Ok, "ready");
            return Some(Ok(()));
        }
        Progress::Failed(step) => {
            let mut detail = format!("failed: {step}");
            if !tail.is_empty() {
                detail = format!("{detail}\n{}", tail.join("\n"));
            }
            return Some(Err(not_ready(cx, w, cx.events.mask(&detail))));
        }
        Progress::Running(step) if *last != now => cx.events.ready(&w.condition, ReadyState::Waiting, &format!("running: {step}")),
        _ => {}
    }
    *last = now;
    None
}

/// The condition failed with `detail`: its event, and the error.
fn not_ready(cx: &Cx<'_>, w: &Wait, detail: String) -> Error {
    cx.events.ready(&w.condition, ReadyState::Failed, &detail);
    Error::NotReady {
        condition: w.condition.clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_first_line_says_how_far_the_bootstrap_got() {
        assert_eq!(progress("ready\n"), Progress::Ready);
        assert_eq!(progress("running: provision/web.sh"), Progress::Running("provision/web.sh".into()));
        assert_eq!(progress("failed: waiting for cache "), Progress::Failed("waiting for cache".into()));
        assert_eq!(progress(""), Progress::Booting, "no file yet");
        assert_eq!(progress("ready now"), Progress::Booting);
    }

    #[test]
    fn a_read_is_the_files_line_then_the_logs_tail() {
        let cmd = read_command("/var/lib/isoloom/ready", Some("/var/log/cloud-init-output.log"));
        assert_eq!(
            cmd,
            "head -n 1 '/var/lib/isoloom/ready' 2>/dev/null; echo --isoloom-log--; tail -n 25 '/var/log/cloud-init-output.log' 2>/dev/null; true"
        );
        let out: Vec<String> = ["failed: provision/web.sh", LOG_MARK, "", "E: Unable to locate package nginx"]
            .map(String::from)
            .into();
        assert_eq!(
            split(&out),
            ("failed: provision/web.sh".into(), vec!["E: Unable to locate package nginx".to_owned()])
        );
        assert_eq!(split(&[LOG_MARK.to_owned()]), (String::new(), Vec::new()), "no file yet");
    }

    #[test]
    fn a_wait_out_of_time_says_why_its_last_read_failed() {
        let w = Wait {
            condition: "ready_file 10.0.0.9".into(),
            login: Login::new("isoloom", "10.0.0.9", None, "").unwrap(),
            path: "/var/lib/isoloom/ready".into(),
            log: None,
            within: Duration::from_secs(900),
            unreachable: None,
            every: Duration::from_secs(5),
        };
        let running = Progress::Running("provision/web.sh".into());
        assert_eq!(out_of_time(&w, &running, None), "not done within 900s (last step: provision/web.sh)");
        assert_eq!(
            out_of_time(&w, &Progress::Booting, Some("Connection reset (exited with code 255)")),
            "not done within 900s (last step: booting); its last read over SSH failed: Connection reset (exited with code 255)"
        );
    }

    #[test]
    fn a_failed_read_is_its_last_error_line_and_exit_status() {
        let out = |stderr: &[&str], code| Captured {
            exit: crate::process::Exit::new(code),
            stdout: Vec::new(),
            stderr: stderr.iter().map(|l| (*l).to_owned()).collect(),
        };
        assert_eq!(
            read_failure(&out(&["Warning: x", "Permission denied (publickey).", ""], Some(255))),
            "Permission denied (publickey). (exited with code 255)"
        );
        assert_eq!(read_failure(&out(&[], None)), "exited with a signal");
    }

    proptest! {
        #[test]
        fn any_line_reads_as_some_progress(line in ".{0,60}") {
            let p = progress(&line);
            if line.trim() == "ready" { prop_assert_eq!(p, Progress::Ready) }
        }
    }
}
