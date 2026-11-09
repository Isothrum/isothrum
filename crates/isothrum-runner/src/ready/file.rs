//! The `ready_file` condition: a run host's bootstrap writes its progress to a file in the
//! `isoloom-ready/1` grammar (`running: <step>`, `failed: <step>`, `ready`; no file yet: still
//! booting), read over SSH every `interval_s` with the bootstrap log's tail. A host that never
//! answers is a failure after `unreachable_timeout_s`, never a success.

use std::time::Duration;

use crate::Result;
use crate::error::Error;
use crate::events::ReadyState;
use crate::executor::{Cx, Echo, call};
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
/// the bootstrap runs out of time, or when the host never answered.
pub(crate) async fn wait(cx: &Cx<'_>, w: &Wait) -> Result<()> {
    let spec = ssh::spec(cx, &w.login, &read_command(&w.path, w.log.as_deref()), Vec::new())?;
    let started = tokio::time::Instant::now();
    let unreachable = w.unreachable.unwrap_or(UNREACHABLE_DEFAULT);
    let (mut reached, mut last) = (false, Progress::Booting);
    loop {
        let read = call(cx, &w.condition, &spec, READ_LIMIT, Echo::Quiet).await;
        match read {
            Ok(out) if out.exit.is_some_and(crate::process::Exit::success) => {
                reached = true;
                let (line, tail) = split(&out.stdout);
                if let Some(done) = settle(cx, w, progress(&line), &tail, &mut last) {
                    return done;
                }
            }
            Err(e @ Error::ToolMissing { .. }) => return Err(e),
            _ if !reached && started.elapsed() >= unreachable => {
                let detail = format!("{} didn't answer over SSH within {}s", w.login.target, unreachable.as_secs());
                return Err(not_ready(cx, w, detail));
            }
            _ => {}
        }
        if started.elapsed() >= w.within {
            let step = match &last {
                Progress::Running(s) => s.as_str(),
                _ => "booting",
            };
            return Err(not_ready(cx, w, format!("not done within {}s (last step: {step})", w.within.as_secs())));
        }
        tokio::time::sleep(w.every).await;
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

    proptest! {
        #[test]
        fn any_line_reads_as_some_progress(line in ".{0,60}") {
            let p = progress(&line);
            if line.trim() == "ready" { prop_assert_eq!(p, Progress::Ready) }
        }
    }
}
