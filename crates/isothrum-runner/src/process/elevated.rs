//! Hyper-V's elevation, through the process port. Hyper-V takes management commands from an
//! administrator only (or a member of "Hyper-V Administrators"), and Vagrant's Hyper-V provider
//! refuses to run otherwise. An operation on such a run gets an [`Elevated`] process runner: when
//! this account may already manage Hyper-V, it runs the tools as they are; else its first call
//! starts one elevated broker (one UAC prompt per operation, never one per command), and every
//! call goes through the session's folder in the run's state (an elevated process inherits
//! neither pipes nor the environment). No `unsafe`: the prompt is `Start-Process -Verb RunAs`.
//!
//! A session's folder is `<run state>/elevated/<token>/`, the token random per session. Opening a
//! session clears `elevated/` first, so nothing an earlier session left (its `ready`, an
//! `exit-1`) is ever read as this one's. Request `n` is two owner-only files: `env-<n>.json`
//! (its variables, values included), then `req-<n>.json` (the program, its arguments and folder,
//! the session's token and `n`; no value of any variable). The broker runs only the request it
//! expects next, with this session's token (it deletes any other unrun), reads and deletes both
//! files before it starts the program, and answers in `out-<n>.log` and `exit-<n>`, which the
//! runner deletes once read. When the session ends, its folder goes.
//!
//! Why a file for the values, and not a pipe: the elevated broker is started by the shell (the
//! UAC prompt), which hands it no handle of its parent, so no inherited pipe; and a named pipe
//! would need a security descriptor of its own (raw FFI, which this crate forbids), as its
//! default one lets every local account connect. The file sits in the run's owner-only state
//! folder from just before the broker picks the request up to the moment it has read it.

mod log;
mod script;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use self::log::LogReader;
use super::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, capture};
use crate::BoxFuture;

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
    /// The broker answers in the session's folder `dir`, to requests carrying `token`.
    Broker {
        /// The session's folder.
        dir: PathBuf,
        /// The session's random token.
        token: String,
    },
    /// The broker couldn't start, or was ended: every call fails with why.
    Failed(String),
}

/// A process runner that elevates, for one operation.
pub(crate) struct Elevated {
    inner: Arc<dyn ProcessRunner>,
    /// The run's `elevated/` folder, holding the session's.
    root: PathBuf,
    session: Mutex<Session>,
    next: AtomicU64,
    /// The variables an elevated process starts without: `PATH`, `VAGRANT_*`, `VAGRANT_HOME`, the
    /// proxies and CA bundles.
    forwarded: Vec<(String, String)>,
}

impl std::fmt::Debug for Elevated {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Elevated").field("root", &self.root).finish_non_exhaustive()
    }
}

impl Elevated {
    /// A session under `root` (the run's state), its calls otherwise through `inner`; `own` is this
    /// process's environment, whose `PATH`, Vagrant and network variables the broker gets.
    pub(crate) fn new(inner: Arc<dyn ProcessRunner>, root: PathBuf, own: impl IntoIterator<Item = (String, String)>) -> Elevated {
        Elevated {
            inner,
            root,
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
            Session::Broker { dir, token } => self.request(&dir, &token, spec, on_line).await,
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
        self.start_broker().await.unwrap_or_else(Session::Failed)
    }

    /// Starts the broker in a fresh session folder, once the account approved the prompt.
    async fn start_broker(&self) -> Result<Session, String> {
        let token = token().map_err(|e| format!("no random session token: {e}"))?;
        let dir = self.root.join(&token);
        fresh_session_dir(&self.root, &dir).map_err(|e| format!("{}: {e}", self.root.display()))?;
        let launcher = script::launcher(&script::broker(&dir, &token, std::process::id()));
        let start = ProcessSpec::new("powershell", ["-NoProfile".to_owned(), "-NonInteractive".into(), "-Command".into(), launcher]);
        match capture(self.inner.as_ref(), &start).await {
            Ok(out) if out.succeeded() => {}
            Ok(out) => return Err(declined(&out.stderr.join("\n"))),
            Err(e) => return Err(e.to_string()),
        }
        if wait_for(&dir.join("ready"), PROMPT_TIMEOUT).await {
            Ok(Session::Broker { dir, token })
        } else {
            Err("the elevated session didn't start".into())
        }
    }

    /// One request through the broker in `dir`: its lines as they come, then its exit code, its
    /// answer's files deleted once read. Dropped before it ends (a timeout), the broker's tree is
    /// ended and the session with it.
    async fn request(&self, dir: &Path, token: &str, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(why) = refusal(spec) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                why,
            });
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let io = |source| ProcessError::Io {
            program: spec.program.clone(),
            source,
        };
        // The values first: the broker takes a request only once its `req` file is there.
        crate::store::write_private(&dir.join(format!("env-{n}.json")), env_file(token, n, spec, &self.forwarded).as_bytes()).map_err(io)?;
        crate::store::write_private(&dir.join(format!("req-{n}.json")), request(token, n, spec).as_bytes()).map_err(io)?;
        let mut guard = Cancel {
            dir,
            armed: true,
            session: &self.session,
        };
        let mut log_reader = LogReader::default();
        let (log, exit) = (dir.join(format!("out-{n}.log")), dir.join(format!("exit-{n}")));
        loop {
            let done = std::fs::read_to_string(&exit).ok();
            log_reader.read(&log, on_line).map_err(io)?;
            if let Some(code) = done {
                log_reader.finish(on_line);
                guard.armed = false;
                answered(dir, n).map_err(io)?;
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
        if let Session::Broker { dir, .. } = self.session() {
            // Between two requests: the broker ends on its own once it reads `done` (or finds its
            // folder gone).
            signal(&dir, "done");
        }
        // What the session wrote goes with it (nothing is there when it ran direct). The next
        // session clears what can't be removed now (a file the broker still holds).
        if let Err(e) = crate::store::remove_path(&self.root) {
            tracing::warn!(error = %e, folder = %self.root.display(), "the elevated session's folder couldn't be removed");
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

/// Empties `root` of every earlier session, then creates the session's folder `dir` in it,
/// owner-only.
fn fresh_session_dir(root: &Path, dir: &Path) -> std::io::Result<()> {
    crate::store::remove_path(root)?;
    crate::store::private_dir(dir)
}

/// Deletes what request `n` left in `dir` once its answer is read: its output, its exit code, and
/// its variables (the broker deleted them already, unless it refused the request).
fn answered(dir: &Path, n: u64) -> std::io::Result<()> {
    for file in [format!("out-{n}.log"), format!("exit-{n}"), format!("env-{n}.json")] {
        crate::store::remove_path(&dir.join(file))?;
    }
    Ok(())
}

/// A random session token: 128 bits, as lowercase hex.
fn token() -> Result<String, getrandom::Error> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)?;
    Ok(bytes.iter().fold(String::with_capacity(32), |mut hex, b| {
        hex.extend(char::from_digit(u32::from(b >> 4), 16));
        hex.extend(char::from_digit(u32::from(b & 15), 16));
        hex
    }))
}

/// Why the broker can't carry `spec` as asked, if it can't: an argument PowerShell would mangle,
/// a standard input (the broker starts the program with none, so it would be dropped).
fn refusal(spec: &ProcessSpec) -> Option<String> {
    if let Some(arg) = spec.args.iter().find(|a| !script::passes_intact(a)) {
        return Some(format!("its argument `{}` wouldn't reach it intact through PowerShell", arg.escape_debug()));
    }
    spec.stdin
        .as_ref()
        .map(|stdin| format!("an elevated call can't be given a standard input ({})", stdin.display()))
}

/// Whether `whoami /groups /fo csv /nh` lists a group that may manage Hyper-V.
fn may_manage(groups: &str) -> bool {
    groups.lines().any(|l| MAY_MANAGE.iter().any(|sid| l.contains(&format!("\"{sid}\""))))
}

/// The variables of `own` an elevated process needs: `PATH` (the tools found here are found
/// there), every `VAGRANT_*`, `VAGRANT_HOME` pinned to this account's boxes, whoever approved,
/// and the proxies and CA bundles (a box download goes out the way this account's does).
fn forwarded(own: impl IntoIterator<Item = (String, String)>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut profile = None;
    for (k, v) in own {
        let upper = k.to_ascii_uppercase();
        if upper == "USERPROFILE" {
            profile = Some(v.clone());
        }
        if upper == "PATH" || upper.starts_with("VAGRANT_") || crate::network_env::is_network_var(&k) {
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

/// Request `n`'s file: the program, its arguments and folder, under the session's `token`. No
/// value of any variable: those are in its [`env_file`].
fn request(token: &str, n: u64, spec: &ProcessSpec) -> String {
    serde_json::json!({
        "session": token,
        "seq": n,
        "program": spec.program,
        "args": spec.args,
        "cwd": spec.cwd.as_ref().map_or_else(|| ".".into(), |c| c.display().to_string()),
    })
    .to_string()
}

/// Request `n`'s variables, under the session's `token`: the forwarded ones, then the call's own
/// (which win).
fn env_file(token: &str, n: u64, spec: &ProcessSpec, forwarded: &[(String, String)]) -> String {
    let mut env: Vec<(String, String)> = forwarded.to_vec();
    for (k, v) in &spec.env {
        env.retain(|(o, _)| !o.eq_ignore_ascii_case(k));
        env.push((k.clone(), v.clone()));
    }
    serde_json::json!({
        "session": token,
        "seq": n,
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
