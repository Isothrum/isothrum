//! Hyper-V's elevation, through the process port. Hyper-V takes management commands from an
//! administrator only (or a member of "Hyper-V Administrators"), and Vagrant's Hyper-V provider
//! refuses to run otherwise. An operation on such a run gets an [`Elevated`] process runner: when
//! this account may already manage Hyper-V, it runs the tools as they are; else its first call
//! starts one elevated broker (one UAC prompt per operation, never one per command), and every
//! call goes through the broker's session folder in the run's state: a request file in, the
//! output lines and the exit code out (an elevated process inherits neither pipes nor the
//! environment). No `unsafe`: the prompt is `Start-Process -Verb RunAs`.

mod script;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, capture};
use crate::BoxFuture;
use crate::events::Stream;

/// How often the session folder is read.
const POLL: Duration = Duration::from_millis(200);

/// How long the broker may take to answer (the account approving the prompt).
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

/// The groups that may manage Hyper-V: an elevated token (High Mandatory Level) or "Hyper-V
/// Administrators".
const MAY_MANAGE: [&str; 2] = ["S-1-16-12288", "S-1-5-32-578"];

/// Where the session stands.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Session {
    /// Not decided yet.
    Unchecked,
    /// The account may manage Hyper-V: the tools run as they are.
    Direct,
    /// The broker answers in the session folder.
    Broker,
    /// The broker couldn't start, or was ended: every call fails with why.
    Failed(String),
}

/// A process runner that elevates, for one operation.
pub(crate) struct Elevated {
    inner: Arc<dyn ProcessRunner>,
    dir: PathBuf,
    session: Mutex<Session>,
    next: AtomicU64,
    /// The variables an elevated process starts without: `PATH`, `VAGRANT_*`, `VAGRANT_HOME`.
    forwarded: Vec<(String, String)>,
}

impl std::fmt::Debug for Elevated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Elevated").field("dir", &self.dir).finish_non_exhaustive()
    }
}

impl Elevated {
    /// A session in `dir` (the run's state), its calls otherwise through `inner`; `own` is this
    /// process's environment, whose `PATH` and Vagrant variables the broker gets.
    pub(crate) fn new(inner: Arc<dyn ProcessRunner>, dir: PathBuf, own: impl IntoIterator<Item = (String, String)>) -> Elevated {
        Elevated {
            inner,
            dir,
            session: Mutex::new(Session::Unchecked),
            next: AtomicU64::new(1),
            forwarded: forwarded(own),
        }
    }

    fn session(&self) -> Session {
        self.session.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn set(&self, s: Session) {
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = s;
    }

    async fn run_spec(&self, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if self.session() == Session::Unchecked {
            let s = self.open().await;
            self.set(s);
        }
        match self.session() {
            Session::Direct | Session::Unchecked => self.inner.run(spec, on_line).await,
            Session::Failed(reason) => Err(ProcessError::Elevation { reason }),
            Session::Broker => self.request(spec, on_line).await,
        }
    }

    /// Decides the session: direct when the account may manage Hyper-V, else the broker started.
    async fn open(&self) -> Session {
        let whoami = ProcessSpec::new("whoami", ["/groups", "/fo", "csv", "/nh"]);
        if let Ok(out) = capture(self.inner.as_ref(), &whoami).await
            && may_manage(&out.stdout.join("\n"))
        {
            return Session::Direct;
        }
        if let Err(e) = crate::store::private_dir(&self.dir) {
            return Session::Failed(format!("{}: {e}", self.dir.display()));
        }
        let launcher = script::launcher(&script::broker(&self.dir, std::process::id()));
        let start = ProcessSpec::new("powershell", ["-NoProfile".to_owned(), "-NonInteractive".into(), "-Command".into(), launcher]);
        match capture(self.inner.as_ref(), &start).await {
            Ok(out) if out.exit.is_some_and(Exit::success) => {}
            Ok(out) => return Session::Failed(declined(&out.stderr.join("\n"))),
            Err(e) => return Session::Failed(e.to_string()),
        }
        if wait_for(&self.dir.join("ready"), PROMPT_TIMEOUT).await {
            Session::Broker
        } else {
            Session::Failed("the elevated session didn't start".into())
        }
    }

    /// One request through the broker: its lines as they come, then its exit code. Dropped before
    /// it ends (a timeout), the broker's tree is ended and the session with it.
    async fn request(&self, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(arg) = spec.args.iter().find(|a| !script::passes_intact(a)) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                arg: arg.escape_debug().to_string(),
            });
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let io = |source| ProcessError::Io {
            program: spec.program.clone(),
            source,
        };
        crate::store::write_private(&self.dir.join(format!("req-{n}.json")), request(spec, &self.forwarded).as_bytes()).map_err(io)?;
        let mut guard = Cancel {
            dir: &self.dir,
            armed: true,
            session: &self.session,
        };
        let mut tail = Tail::default();
        let (log, exit) = (self.dir.join(format!("out-{n}.log")), self.dir.join(format!("exit-{n}")));
        loop {
            let done = std::fs::read_to_string(&exit).ok();
            tail.read(&log, on_line).map_err(io)?;
            if let Some(code) = done {
                tail.finish(on_line);
                guard.armed = false;
                return Ok(Exit::new(code.trim().parse().ok()));
            }
            tokio::time::sleep(POLL).await;
        }
    }
}

impl ProcessRunner for Elevated {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        Box::pin(self.run_spec(spec, on_line))
    }
}

impl Drop for Elevated {
    fn drop(&mut self) {
        if self.session() == Session::Broker {
            // Between two requests: the broker ends on its own once it reads `done`.
            signal(&self.dir, "done");
        }
    }
}

/// A request in flight: dropped while armed (its future cut short), it ends the broker's tree.
struct Cancel<'a> {
    dir: &'a Path,
    armed: bool,
    session: &'a Mutex<Session>,
}

impl Drop for Cancel<'_> {
    fn drop(&mut self) {
        if self.armed {
            signal(self.dir, "cancel");
            *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Session::Failed("the elevated session was ended".into());
        }
    }
}

/// Tells the broker `what` (`done`, `cancel`) through its file in the session's folder. A broker
/// already gone can't be told anything, and a drop has no one to report to: it is traced.
fn signal(dir: &Path, what: &str) {
    if let Err(e) = std::fs::write(dir.join(what), "") {
        tracing::warn!(error = %e, signal = what, "the elevated session couldn't be told");
    }
}

/// A growing log, read a line at a time from where the last read stopped.
#[derive(Debug, Default)]
struct Tail {
    offset: u64,
    partial: Vec<u8>,
}

impl Tail {
    /// Hands `on_line` every whole line `log` gained since the last read.
    fn read(&mut self, log: &Path, on_line: OnLine<'_>) -> std::io::Result<()> {
        let mut f = match std::fs::File::open(log) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        let read = f.read_to_end(&mut bytes)?;
        self.offset += u64::try_from(read).unwrap_or(0);
        self.partial.extend(bytes);
        while let Some(end) = self.partial.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            on_line(Stream::Stdout, String::from_utf8_lossy(&line).trim_end_matches(['\r', '\n']));
        }
        Ok(())
    }

    /// The last line, when the log didn't end with a line break.
    fn finish(&mut self, on_line: OnLine<'_>) {
        if !self.partial.is_empty() {
            on_line(
                Stream::Stdout,
                String::from_utf8_lossy(&std::mem::take(&mut self.partial)).trim_end_matches('\r'),
            );
        }
    }
}

/// Whether `whoami /groups /fo csv /nh` lists a group that may manage Hyper-V.
fn may_manage(groups: &str) -> bool {
    groups.lines().any(|l| MAY_MANAGE.iter().any(|sid| l.contains(&format!("\"{sid}\""))))
}

/// The variables of `own` an elevated process needs: `PATH` (the tools found here are found
/// there), every `VAGRANT_*`, and `VAGRANT_HOME` pinned to this account's boxes, whoever approved.
fn forwarded(own: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut profile = None;
    for (k, v) in own {
        let upper = k.to_ascii_uppercase();
        if upper == "USERPROFILE" {
            profile = Some(v.clone());
        }
        if upper == "PATH" || upper.starts_with("VAGRANT_") {
            out.push((k, v));
        }
    }
    if !out.iter().any(|(k, _)| k.eq_ignore_ascii_case("VAGRANT_HOME"))
        && let Some(p) = profile
    {
        out.push(("VAGRANT_HOME".into(), format!("{}\\.vagrant.d", p.trim_end_matches(['\\', '/']))));
    }
    out
}

/// A request's JSON: the program, its arguments and folder, and its environment (the forwarded
/// variables, then the call's own).
fn request(spec: &ProcessSpec, forwarded: &[(String, String)]) -> String {
    let mut env: Vec<(String, String)> = forwarded.to_vec();
    for (k, v) in &spec.env {
        env.retain(|(o, _)| !o.eq_ignore_ascii_case(k));
        env.push((k.clone(), v.clone()));
    }
    serde_json::json!({
        "program": spec.program,
        "args": spec.args,
        "cwd": spec.cwd.as_ref().map_or_else(|| ".".into(), |c| c.display().to_string()),
        "env": env.iter().map(|(k, v)| serde_json::json!({ "k": k, "v": v })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// What a failed elevation's error says: declining the prompt is said plainly.
fn declined(stderr: &str) -> String {
    if stderr.contains("canceled by the user") || stderr.contains("cancelled by the user") {
        "the Windows permission prompt was declined".into()
    } else {
        format!("the elevated session didn't start: {}", stderr.trim())
    }
}

/// Whether `file` appears within `limit`.
async fn wait_for(file: &Path, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if file.exists() {
            return true;
        }
        tokio::time::sleep(POLL).await;
    }
    file.exists()
}

#[cfg(test)]
mod tests;
