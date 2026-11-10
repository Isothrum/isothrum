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
//! runner deletes once read. When the session ends, its folder goes. A broker that stops other
//! than at the session's end (its watcher ending it, its loop broken) says why in `ended`: the
//! request in flight fails with that reason at once, and so does every later call. So does a
//! request that fails before its answer is read (its file not written, its answer unreadable),
//! since the broker would wait for it and take no later one.
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
use super::{Captured, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, capture};
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
    /// A session under `root` (the run's state), its calls otherwise through `inner`; the broker,
    /// which inherits no environment, gets `forwarded` with each request
    /// ([`HostEnv::for_elevation`](crate::host_env::HostEnv::for_elevation)).
    pub(crate) fn new(inner: Arc<dyn ProcessRunner>, root: PathBuf, forwarded: Vec<(String, String)>) -> Elevated {
        Elevated {
            inner,
            root,
            session: Mutex::new(Session::Unchecked),
            next: AtomicU64::new(1),
            forwarded,
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

    /// Decides the session: direct when the account may manage Hyper-V, else the broker started
    /// (a `whoami` that can't say is traced by [`may_manage_hyperv`]).
    async fn open(&self) -> Session {
        if may_manage_hyperv(self.inner.as_ref()).await == Some(true) {
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
            Ok(out) => return Err(declined(&out)),
            Err(e) => return Err(e.to_string()),
        }
        if wait_for(&dir.join("ready"), PROMPT_TIMEOUT).await {
            Ok(Session::Broker { dir, token })
        } else {
            Err("the elevated session didn't start".into())
        }
    }

    /// One request through the broker in `dir`: its lines as they come, then its exit code, its
    /// answer's files deleted once read. Dropped before it ends (a timeout), or failed before its
    /// answer (a request file that couldn't be written, the broker ended), it ends the broker's
    /// tree and the session with it: the broker waits for this request's turn, and would take no
    /// later one.
    async fn request(&self, dir: &Path, token: &str, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(why) = refusal(spec) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                why,
            });
        }
        let mut guard = Cancel {
            dir,
            armed: true,
            session: &self.session,
        };
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let io = |source| ProcessError::Io {
            program: spec.program.clone(),
            source,
        };
        // The values first: the broker takes a request only once its `req` file is there.
        crate::store::write_private(&dir.join(format!("env-{n}.json")), env_file(token, n, spec, &self.forwarded).as_bytes()).map_err(io)?;
        crate::store::write_private(&dir.join(format!("req-{n}.json")), request_file(token, n, spec).as_bytes()).map_err(io)?;
        let mut log_reader = LogReader::default();
        let (log, exit) = (dir.join(format!("out-{n}.log")), dir.join(format!("exit-{n}")));
        loop {
            let done = read_if_there(&exit).map_err(io)?;
            log_reader.read(&log, on_line).map_err(io)?;
            if let Some(code) = done {
                log_reader.finish(on_line);
                guard.armed = false;
                answered(dir, n).map_err(io)?;
                return exit_code(&code).map(|c| Exit::new(Some(c))).map_err(io);
            }
            if let Some(reason) = ended(dir).map_err(io)? {
                // Nothing is left to cancel: the later calls fail with why.
                guard.armed = false;
                self.set(Session::Failed(reason.clone()));
                return Err(ProcessError::Elevation { reason });
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

/// `file`'s text, `None` while it isn't there.
fn read_if_there(file: &Path) -> std::io::Result<Option<String>> {
    crate::store::if_there(std::fs::read_to_string(file))
}

/// Why the session in `dir` ended, once it has: the reason its broker gave (in `ended`, written
/// when it stops other than at `done`), or its folder gone.
fn ended(dir: &Path) -> std::io::Result<Option<String>> {
    if let Some(reason) = read_if_there(&dir.join("ended"))? {
        return Ok(Some(format!("the elevated session ended: {}", reason.trim())));
    }
    Ok((!dir.try_exists()?).then(|| "the elevated session ended: its folder is gone".to_owned()))
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

/// The exit code an `exit-<n>` file holds: the broker always writes a number, so anything else
/// is a broken answer, never a program ended by a signal.
fn exit_code(text: &str) -> std::io::Result<i32> {
    text.trim().parse().map_err(|_| {
        let why = format!("the elevated broker answered `{}`, not an exit code", text.trim().escape_debug());
        std::io::Error::new(std::io::ErrorKind::InvalidData, why)
    })
}

/// Whether this account may manage Hyper-V, as Vagrant's Hyper-V provider requires for every
/// command on its machines (`vagrant status` included): its token is elevated, or it is a member of
/// "Hyper-V Administrators". Read from `whoami /groups`, run through `processes` (no `unsafe`).
/// `None` when `whoami` can't say (it didn't run, or failed), which is traced.
pub async fn may_manage_hyperv(processes: &dyn ProcessRunner) -> Option<bool> {
    let whoami = ProcessSpec::new("whoami", ["/groups", "/fo", "csv", "/nh"]);
    match capture(processes, &whoami).await {
        Ok(out) if out.succeeded() => Some(may_manage(&out.stdout.join("\n"))),
        Ok(out) => {
            tracing::warn!(stderr = %out.stderr.join("\n"), "`whoami /groups` failed: whether this account may manage Hyper-V is unknown");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "`whoami /groups` didn't run: whether this account may manage Hyper-V is unknown");
            None
        }
    }
}

/// Whether `whoami /groups /fo csv /nh` lists a group that may manage Hyper-V.
fn may_manage(groups: &str) -> bool {
    groups.lines().any(|l| MAY_MANAGE.iter().any(|sid| l.contains(&format!("\"{sid}\""))))
}

/// Request `n`'s file: the program, its arguments and folder, under the session's `token`. No
/// value of any variable: those are in its [`env_file`].
fn request_file(token: &str, n: u64, spec: &ProcessSpec) -> String {
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
fn declined(out: &Captured) -> String {
    let cancelled = |l: &String| l.contains("canceled by the user") || l.contains("cancelled by the user");
    if out.stderr.iter().any(cancelled) {
        "the Windows permission prompt was declined".into()
    } else {
        format!("the elevated session didn't start: {}", out.reason())
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
