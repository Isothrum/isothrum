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
//! request in flight fails with that reason at once, and so does every later call.
//!
//! Only Hyper-V's programs go through the broker (Vagrant and Windows PowerShell,
//! [`protocol::PROGRAMS`]); every other program of the operation runs as the account. The broker
//! finds each program itself, at the path an administrator installed it to, and takes none of the
//! variables that choose which code it loads ([`protocol::passes`]): the session's folder belongs
//! to the account, so what a request names is never trusted to pick the program or its loader.
//!
//! One request failing or cut short never ends the session: a request whose future is dropped
//! (its step's timeout, a probe given up on) or that fails before its answer is read (its file
//! not written, its answer unreadable) writes `cancel-<n>`. The broker ends that request's
//! process tree, or skips it if its turn hasn't come, and takes the next: the requests waiting
//! behind it, and every later one, run in the same session (one UAC prompt per operation).
//!
//! Why a file for the values, and not a pipe: the elevated broker is started by the shell (the
//! UAC prompt), which hands it no handle of its parent, so no inherited pipe; and a named pipe
//! would need a security descriptor of its own (raw FFI, which this crate forbids), as its
//! default one lets every local account connect. The file sits in the run's owner-only state
//! folder from just before the broker picks the request up to the moment it has read it.

mod log;
mod protocol;
mod script;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use self::log::LogReader;
use super::{Captured, Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, capture};
use crate::BoxFuture;

/// The command that runs `script` (Windows PowerShell) elevated once, waiting for its end: one
/// UAC prompt, its exit code the script's, [`DECLINED`] when the prompt was declined. For one
/// administrator's step outside an operation's session (VMware's host adapter given its address).
pub(crate) fn once(script: &str) -> String {
    script::waited(script)
}

/// The exit code of [`once`] whose prompt was declined.
pub(crate) const DECLINED: i32 = script::ERROR_CANCELLED;

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
    /// Held while the session is decided: two first calls at once would each start a broker (two
    /// prompts, the first session leaked); the second waits and finds the session decided.
    opening: tokio::sync::Mutex<()>,
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
            opening: tokio::sync::Mutex::new(()),
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
        // Only what Hyper-V needs an administrator for is elevated: any other program runs as the
        // account, as it would without Hyper-V, and never opens (or waits for) the session.
        if !protocol::elevates(&spec.program) {
            return self.inner.run(spec, on_line).await;
        }
        if self.session() == Session::Unchecked {
            let _opening = self.opening.lock().await;
            if self.session() == Session::Unchecked {
                let s = self.open().await;
                self.set(s);
            }
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
        if may_manage_hyperv(self.inner.as_ref()).await == HyperVRights::Granted {
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
            Ok(out) if out.success() => {}
            Ok(out) => return Err(declined(&out)),
            Err(e) => return Err(e.to_string()),
        }
        if wait_for(&dir.join(protocol::READY), PROMPT_TIMEOUT).await {
            Ok(Session::Broker { dir, token })
        } else {
            Err("the elevated session didn't start".into())
        }
    }

    /// One request through the broker in `dir`: its lines as they come, then its exit code, its
    /// answer's files deleted once read. Dropped before it ends (a timeout), or failed before its
    /// answer (a request file that couldn't be written, an answer that couldn't be read), it
    /// cancels itself alone ([`Cancel`]): the session and the requests behind it go on.
    async fn request(&self, dir: &Path, token: &str, spec: &ProcessSpec, on_line: OnLine<'_>) -> Result<Exit, ProcessError> {
        if let Some(why) = refusal(spec) {
            return Err(ProcessError::Refused {
                program: spec.program.clone(),
                why,
            });
        }
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        let mut guard = Cancel { dir, n, armed: true };
        let io = |source| ProcessError::Io {
            program: spec.program.clone(),
            source,
        };
        // The values first: the broker takes a request only once its `req` file is there.
        crate::store::write_private(&dir.join(protocol::variables(n)), env_file(token, n, spec, &self.forwarded).as_bytes()).map_err(io)?;
        crate::store::write_private(&dir.join(protocol::request(n)), request_file(token, n, spec).as_bytes()).map_err(io)?;
        let mut log_reader = LogReader::default();
        let (log, exit) = (dir.join(protocol::output(n)), dir.join(protocol::exit(n)));
        loop {
            let done = read_if_there(&exit).map_err(io)?;
            log_reader.read(&log, on_line).map_err(io)?;
            if let Some(code) = done {
                log_reader.finish(on_line);
                guard.armed = false;
                remove_request_files(dir, n).map_err(io)?;
                return exit_code(&code).map(|c| Exit::new(Some(c))).map_err(io);
            }
            if let Some(reason) = session_end_reason(dir).map_err(io)? {
                // The broker is gone, nothing is left to cancel: the later calls fail with why.
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
            signal(&dir, protocol::DONE);
        }
        // What the session wrote goes with it (nothing is there when it ran direct). The next
        // session clears what can't be removed now (a file the broker still holds).
        if let Err(e) = crate::store::remove_path(&self.root) {
            tracing::warn!(error = %e, folder = %self.root.display(), "the elevated session's folder couldn't be removed");
        }
    }
}

/// Request `n` in flight: dropped while armed (its future cut short, or failed before its answer
/// was read), it cancels that request alone through `cancel-<n>`. The broker ends its process
/// tree, or skips it unrun, and deletes its files; the session stays open for the next request.
struct Cancel<'a> {
    dir: &'a Path,
    n: u64,
    armed: bool,
}

impl Drop for Cancel<'_> {
    fn drop(&mut self) {
        if self.armed {
            let cancel = protocol::cancel(self.n);
            signal(self.dir, &cancel);
            // An answer that came just before the cancel: the broker has moved on and won't read
            // `cancel-<n>`, so the request's files are this guard's to remove.
            if self.dir.join(protocol::exit(self.n)).exists()
                && let Err(e) = remove_request_files(self.dir, self.n).and_then(|()| crate::store::remove_path(&self.dir.join(&cancel)))
            {
                tracing::warn!(error = %e, "a cancelled elevated request's answer couldn't be removed");
            }
        }
    }
}

/// `file`'s text, `None` while it isn't there.
fn read_if_there(file: &Path) -> std::io::Result<Option<String>> {
    crate::store::if_there(std::fs::read_to_string(file))
}

/// Why the session in `dir` ended, once it has: the reason its broker gave (in `ended`, written
/// when it stops other than at `done`), or its folder gone.
fn session_end_reason(dir: &Path) -> std::io::Result<Option<String>> {
    if let Some(reason) = read_if_there(&dir.join(protocol::ENDED))? {
        return Ok(Some(format!("the elevated session ended: {}", reason.trim())));
    }
    Ok((!dir.try_exists()?).then(|| "the elevated session ended: its folder is gone".to_owned()))
}

/// Tells the broker `what` ([`protocol::DONE`], a [`protocol::cancel`]) through its file in the session's folder. A broker
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
fn remove_request_files(dir: &Path, n: u64) -> std::io::Result<()> {
    for file in [protocol::output(n), protocol::exit(n), protocol::variables(n)] {
        crate::store::remove_path(&dir.join(file))?;
    }
    Ok(())
}

/// A random session token: 128 bits, as lowercase hex.
fn token() -> Result<String, getrandom::Error> {
    crate::encoding::random_hex::<16>()
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

/// Whether this account may manage Hyper-V ([`may_manage_hyperv`]'s answer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HyperVRights {
    /// Its token is elevated, or it is a member of "Hyper-V Administrators".
    Granted,
    /// Neither: Vagrant's Hyper-V provider refuses its commands.
    Missing,
    /// `whoami` couldn't say (it didn't run, or failed).
    Unknown,
}

/// Whether this account may manage Hyper-V, as Vagrant's Hyper-V provider requires for every
/// command on its machines (`vagrant status` included): its token is elevated, or it is a member of
/// "Hyper-V Administrators". Read from `whoami /groups`, run through `processes` (no `unsafe`).
/// [`HyperVRights::Unknown`] when `whoami` can't say (it didn't run, or failed), which is traced.
pub async fn may_manage_hyperv(processes: &dyn ProcessRunner) -> HyperVRights {
    let whoami = ProcessSpec::new("whoami", ["/groups", "/fo", "csv", "/nh"]);
    match capture(processes, &whoami).await {
        Ok(out) if out.success() => {
            if may_manage(&out.stdout.join("\n")) {
                HyperVRights::Granted
            } else {
                HyperVRights::Missing
            }
        }
        Ok(out) => {
            tracing::warn!(stderr = %out.stderr_tail.join("\n"), "`whoami /groups` failed: whether this account may manage Hyper-V is unknown");
            HyperVRights::Unknown
        }
        Err(e) => {
            tracing::warn!(error = %e, "`whoami /groups` didn't run: whether this account may manage Hyper-V is unknown");
            HyperVRights::Unknown
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
/// (which win), less those an elevated program is never given ([`protocol::passes`]: its search
/// path, Ruby's load path and options, any `VAGRANT_*` not listed). The broker refuses a request
/// that carries one.
fn env_file(token: &str, n: u64, spec: &ProcessSpec, forwarded: &[(String, String)]) -> String {
    let mut env: Vec<(String, String)> = forwarded.to_vec();
    for (k, v) in &spec.env {
        env.retain(|(o, _)| !o.eq_ignore_ascii_case(k));
        env.push((k.clone(), v.clone()));
    }
    env.retain(|(k, _)| {
        let passes = protocol::passes(k);
        if !passes {
            tracing::debug!(name = %k, "a variable an elevated program isn't given is dropped");
        }
        passes
    });
    serde_json::json!({
        "session": token,
        "seq": n,
        "env": env.iter().map(|(k, v)| serde_json::json!({ "k": k, "v": v })).collect::<Vec<_>>(),
    })
    .to_string()
}

/// What a failed elevation's error says: declining the prompt (the launcher's exit code
/// [`script::ERROR_CANCELLED`], whatever language Windows words it in) is said plainly.
fn declined(out: &Captured) -> String {
    if out.exit.code == Some(script::ERROR_CANCELLED) {
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
