//! The tools an operation runs, kept in the run's state folder while they run, so the next
//! operation can end those a killed process left behind. An embedder keeps the tools it starts
//! itself the same way, in a folder of its own ([`ProcessSpec::ledger_dir`](super::ProcessSpec)),
//! and ends what its previous process left there when it starts ([`end_orphans`]).
//!
//! Each tool runs in a process group of its own (a job object on Windows), so that this process's
//! own stop reaches it gracefully; a process killed outright (SIGKILL, Task Manager, a crash)
//! stops nothing, and its tools ran on to their end: a `docker compose up` finished seconds later,
//! a `vagrant up` can run for an hour, and the next start waited on Vagrant's machine lock.
//!
//! - While a tool runs, its entry `<owner>-<pid>.tool` in the run's `tools/` folder says which
//!   process it is: its id, and its start as the OS has it (Linux: its start in clock ticks since
//!   boot and the boot's id; elsewhere: when it was started, read from this computer's clock the
//!   moment it was, which the OS stored then too). The entry goes when the tool ends (cut short:
//!   once it was ended).
//! - While it runs, the processes of its group other than the tool itself (what it started: a
//!   Terraform provider plugin) are added to its entry, each with its id and start, every few
//!   seconds (`note_group`): the proof, once the tool itself is gone, that its group is the
//!   very one recorded.
//! - `<owner>` is the recording process's token: that process holds `<owner>.owner` locked for as
//!   long as it lives, and the OS releases the lock when it dies, however it dies. An entry whose
//!   owner's lock can be taken is a dead process's.
//! - [`end_orphans`], with the run locked at the start of every operation, ends each tool a dead
//!   process left (its process group asked to stop, as a cut-short tool is, then killed; on
//!   Windows its tree, `taskkill /T /F`), removes the container it ran in when its entry names
//!   one (`docker rm -f`: ending the `docker` client leaves its container running), and removes
//!   the entries. It ends only the very group that was recorded: when the tool is still the
//!   process recorded, its group; when the tool is gone (Terraform killed with its embedder, its
//!   provider plugin left running), its group still, if a member of it recorded is still there,
//!   the very process recorded, and still in it: a group's id isn't another group's while a
//!   process of it lives. A recorded id that now names another process (a later start, another
//!   program), and a group none of whose recorded members is still in it, are left alone.
//! - On Windows a tool's job object already ends its tree when this process dies (the
//!   runner's `job` module), the tool itself there or not: nothing it started is left, and its
//!   entry is the backstop for the tool alone.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
#[cfg(unix)]
use std::time::Duration;

/// The run's folder of tool entries, in its state folder.
pub(crate) const DIR: &str = "tools";

mod group;
mod start;

#[cfg(unix)]
use group::group_members;
use group::members_still_in_group;
pub(crate) use start::os_started_at;
use start::{os_start, recorded_start};

/// The extension of a tool's entry.
const TOOL: &str = "tool";

/// The extension of a console tunnel's entry: a process that lives as long as its console, not
/// to an operation's end.
const TUNNEL: &str = "tunnel";

/// How far apart, in seconds, the start the OS stored and the one recorded may be for one
/// process (both read from the same clock at the same moment; the OS rounds to the second).
const SLACK: u64 = 2;

/// How long a tool left behind has to stop once asked (as a tool cut short has).
#[cfg(unix)]
const GRACE: Duration = super::stopping::GRACE;

/// How often a tool left behind is checked for having stopped.
#[cfg(unix)]
const STOP_POLL: Duration = Duration::from_millis(100);

/// How often a running tool's group is read for the processes it started ([`note_group`]).
#[cfg(unix)]
const GROUP_POLL: Duration = Duration::from_secs(2);

/// A tool's start, as recorded and as the OS has it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    /// Linux: ticks since boot (`/proc/<pid>/stat`), and the boot's id.
    Boot { boot: String, ticks: u64 },
    /// Elsewhere: seconds since the epoch.
    At(u64),
}

impl Start {
    /// The start as the ledger entry writes it.
    fn entry_text(&self) -> String {
        match self {
            Start::Boot { boot, ticks } => format!("boot:{boot}:{ticks}"),
            Start::At(secs) => format!("at:{secs}"),
        }
    }

    /// [`Start::entry_text`] read back.
    fn from_entry_text(word: &str) -> Option<Start> {
        if let Some(rest) = word.strip_prefix("boot:") {
            let (boot, ticks) = rest.rsplit_once(':')?;
            return Some(Start::Boot {
                boot: boot.to_owned(),
                ticks: ticks.parse().ok()?,
            });
        }
        word.strip_prefix("at:")?.parse().ok().map(Start::At)
    }

    /// Whether `now`, the start the OS has for the process, is this one (seconds within
    /// [`SLACK`]): the same process, not another that got the id later.
    fn same_process_as(&self, now: &Start) -> bool {
        match (self, now) {
            (Start::At(a), Start::At(b)) => a.abs_diff(*b) <= SLACK,
            (a, b) => a == b,
        }
    }
}

/// One tool recorded: its entry, removed when this drops, and its id.
#[derive(Debug)]
pub(crate) struct Recorded {
    path: PathBuf,
    /// The tool's id, which Unix reads to note the members of its process group (a Windows job
    /// object ends the tree).
    #[cfg(unix)]
    pid: u32,
}

impl Recorded {
    /// Records the tool `pid` just started in `dir`, with the `container` it runs in, if any. A
    /// record that can't be written is traced: the tool runs unrecorded.
    pub(crate) fn write(dir: &Path, pid: u32, container: Option<&str>) -> Option<Recorded> {
        Recorded::entry(dir, pid, container, TOOL)
    }

    /// Records the console tunnel `pid` (an `ssh` leading its own process group) just started in
    /// `dir`: ended as a tool is when its process dies, and by [`end_tunnels`] (a `down`) whatever
    /// process holds it.
    pub(crate) fn tunnel(dir: &Path, pid: u32) -> Option<Recorded> {
        Recorded::entry(dir, pid, None, TUNNEL)
    }

    /// The entry `<owner>-<pid>.<extension>` of `pid` in `dir`.
    fn entry(dir: &Path, pid: u32, container: Option<&str>, extension: &str) -> Option<Recorded> {
        let start = recorded_start(pid)?;
        let written = owner_token(dir).and_then(|token| {
            let path = dir.join(format!("{token}-{pid}.{extension}"));
            std::fs::write(&path, entry_text(pid, &start, container))?;
            Ok(path)
        });
        match written {
            Ok(path) => Some(Recorded {
                path,
                #[cfg(unix)]
                pid,
            }),
            Err(e) => {
                tracing::warn!(error = %e, dir = %dir.display(), "a tool couldn't be recorded: a killed operation would leave it running");
                None
            }
        }
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        remove_entry(&self.path);
    }
}

/// Adds to `recorded`'s entry, every `GROUP_POLL` (Unix) for as long as it is polled (while its tool
/// runs), each process of the tool's group it hasn't yet: what the tool started, ended with the
/// group by [`end_orphans`] once the tool itself is gone. Never ready; nothing without an entry,
/// and nothing on Windows, whose job object ends the tool's tree however this process dies.
pub(crate) async fn note_group(recorded: Option<&Recorded>) -> Infallible {
    #[cfg(unix)]
    if let Some(recorded) = recorded {
        let mut noted = Vec::new();
        loop {
            tokio::time::sleep(GROUP_POLL).await;
            note_members(&recorded.path, recorded.pid, &mut noted).await;
        }
    }
    #[cfg(not(unix))]
    let _ = recorded;
    std::future::pending().await
}

/// Adds to the entry `path` each process of `leader`'s group not in `noted` yet (and to `noted`).
/// An entry that can't be added to is traced: those processes go unrecorded.
#[cfg(unix)]
async fn note_members(path: &Path, leader: u32, noted: &mut Vec<(u32, Start)>) {
    use std::io::Write as _;
    let new: Vec<(u32, Start)> = group_members(leader).await.into_iter().filter(|m| !noted.contains(m)).collect();
    if new.is_empty() {
        return;
    }
    let lines = new
        .iter()
        .map(|(pid, start)| format!("{pid} {}\n", start.entry_text()))
        .collect::<Vec<_>>()
        .concat();
    let added = File::options().append(true).open(path).and_then(|mut f| f.write_all(lines.as_bytes()));
    match added {
        Ok(()) => noted.extend(new),
        Err(e) => {
            tracing::warn!(error = %e, path = %path.display(), "the processes a tool started couldn't be recorded: a killed operation would leave them running");
        }
    }
}

/// Removes a tool's entry (a running tool's, or one being created) once its use ended; one that
/// can't be removed is traced.
fn remove_entry(path: &Path) {
    if let Err(e) = crate::store::if_there(std::fs::remove_file(path)) {
        tracing::warn!(error = %e, path = %path.display(), "a tool's entry couldn't be removed");
    }
}

/// The locks this process holds as the owner of each folder's entries, with its token.
/// Process-wide on purpose, where the runner keeps its other state per instance: the token stands
/// for the process, whose death is what its owner file's lock tells, so two runners in one
/// process are one owner, with one token and one locked file per folder. The map's lock is held
/// while an owner file is made, a few local file operations (the runner's exception for its state
/// files): only tools of different runs ever wait for it, since a run's folder is used by the one
/// operation holding the run.
static OWNED: Mutex<BTreeMap<PathBuf, (String, File)>> = Mutex::new(BTreeMap::new());

/// This process's token in `dir`: its `<token>.owner` made and locked the first time, held until
/// it exits. Made again when it is no longer there (the run's state folder removed and made again
/// while this process lives, by an embedder or by hand): an entry beside no owner would never be
/// ended.
fn owner_token(dir: &Path) -> io::Result<String> {
    let mut owned = OWNED.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((token, _)) = owned.get(dir) {
        if dir.join(format!("{token}.owner")).is_file() {
            return Ok(token.clone());
        }
        owned.remove(dir);
    }
    crate::store::private_dir(dir)?;
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let token = format!("{:016x}", u64::from_le_bytes(bytes));
    let lock = File::create(dir.join(format!("{token}.owner")))?;
    lock.try_lock().map_err(|e| io::Error::other(e.to_string()))?;
    owned.insert(dir.to_path_buf(), (token.clone(), lock));
    Ok(token)
}

/// Takes the lock `path` when its owner is gone, and returns it (held until `path` goes); `None`
/// while this process, or another live one, holds it.
fn lock_if_owner_gone(path: &Path) -> Option<File> {
    let file = File::options().read(true).write(true).open(path).ok()?;
    file.try_lock().ok().map(|()| file)
}

/// Ends each tool a dead process recorded in `dir` and removes its entries; the ids it ended.
///
/// The runner calls it on a run's folder at the start of every operation. An embedder that records
/// tools of its own in a folder of its own ([`ProcessSpec::ledger_dir`](super::ProcessSpec)) calls
/// it on that folder when it starts, to end what a previous process of its left running.
pub async fn end_orphans(dir: &Path) -> Vec<u32> {
    let mut ended = Vec::new();
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ended,
        Err(e) => {
            tracing::warn!(error = %e, dir = %dir.display(), "the tools' ledger couldn't be read: no orphan is ended");
            return ended;
        }
    };
    let names: Vec<String> = read.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    for token in names.iter().filter_map(|n| n.strip_suffix(".owner")) {
        let owner = dir.join(format!("{token}.owner"));
        let Some(lock) = lock_if_owner_gone(&owner) else { continue };
        let prefix = format!("{token}-");
        for entry in names.iter().filter(|n| n.starts_with(&prefix)) {
            let path = dir.join(entry);
            if Path::new(entry).extension().is_none_or(|x| x != TOOL && x != TUNNEL) {
                continue;
            }
            ended.extend(end_entry(&path).await);
            if let Err(e) = crate::store::if_there(std::fs::remove_file(&path)) {
                tracing::warn!(error = %e, path = %path.display(), "a dead process's tool entry couldn't be removed");
            }
        }
        drop(lock);
        if let Err(e) = crate::store::if_there(std::fs::remove_file(&owner)) {
            tracing::warn!(error = %e, path = %owner.display(), "a dead process's owner file couldn't be removed");
        }
    }
    ended
}

/// Ends every console tunnel recorded in `dir`, whichever process holds it (a `down` from
/// another process than the viewer's), and removes their entries; the ids it ended. The holder's
/// own end of it then finds it gone.
pub async fn end_tunnels(dir: &Path) -> Vec<u32> {
    let mut ended = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else { return ended };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|x| x != TUNNEL) {
            continue;
        }
        ended.extend(end_entry(&path).await);
        remove_entry(&path);
    }
    ended
}

/// An entry's text: the tool's id, its start, and the container it runs in, if any.
fn entry_text(pid: u32, start: &Start, container: Option<&str>) -> String {
    match container {
        Some(name) => format!("{pid} {} {name}\n", start.entry_text()),
        None => format!("{pid} {}\n", start.entry_text()),
    }
}

/// A tool's entry, read back.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    pid: u32,
    start: Start,
    /// The container it runs in, if any.
    container: Option<String>,
    /// The processes of its group it started, each with its start ([`note_group`]).
    members: Vec<(u32, Start)>,
}

/// An entry's text read back: its first line ([`entry_text`]), then a line per member of the
/// tool's group (`note_members`, Unix); a member's line that doesn't read (cut short by a kill) is
/// skipped.
fn parse_entry(text: &str) -> Option<Entry> {
    let mut lines = text.lines();
    let mut words = lines.next()?.split_whitespace();
    let pid = words.next()?.parse().ok()?;
    let start = Start::from_entry_text(words.next()?)?;
    let container = words.next().map(str::to_owned);
    let members = lines
        .filter_map(|line| {
            let (id, started) = line.split_once(' ')?;
            Some((id.parse().ok()?, Start::from_entry_text(started.trim())?))
        })
        .collect();
    Some(Entry {
        pid,
        start,
        container,
        members,
    })
}

/// Ends the group of the tool `path` records, when the tool is still the very process recorded
/// (its id and start), or else when a member of its group recorded is (and is still in it); then
/// removes the container it ran in, which its name marks as this tool's whatever became of the
/// process.
async fn end_entry(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let Entry {
        pid,
        start,
        container,
        members,
    } = parse_entry(&text)?;
    let now = os_start(pid).await;
    let ended = if now.as_ref().is_some_and(|now| start.same_process_as(now)) {
        tracing::warn!(pid, "ending a tool a killed operation left running");
        end_group(pid).await;
        true
    } else if members_still_in_group(pid, &members).await {
        tracing::warn!(pid, "ending what a tool a killed operation left running started, the tool itself gone");
        end_group(pid).await;
        true
    } else {
        tracing::debug!(
            pid,
            "a tool a killed operation recorded is gone, or its id names another process, and nothing it started is left in its group: left alone"
        );
        false
    };
    if let Some(name) = container {
        remove_container(&name).await;
    }
    ended.then_some(pid)
}

/// Removes the container `name` a killed operation's tool ran in (`docker rm -f`); one already
/// gone is traced only.
#[expect(
    clippy::disallowed_methods,
    reason = "the process module's own clean-up of what a killed operation left: it can't go through the runner it cleans up after"
)]
async fn remove_container(name: &str) {
    let mut cmd = tokio::process::Command::new("docker");
    cmd.args(["rm", "-f", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    headless(&mut cmd);
    match cmd.status().await {
        Ok(status) if status.success() => {
            tracing::warn!(container = name, "removed the container of a tool a killed operation left running");
        }
        Ok(status) => {
            tracing::debug!(container = name, code = ?status.code(), "the container of a tool a killed operation recorded wasn't removed (gone already, or no engine)");
        }
        Err(e) => {
            tracing::warn!(container = name, error = %e, "the container of a tool a killed operation recorded couldn't be removed: it may still run");
        }
    }
}

/// Asks the process group `pid` leads to stop (SIGINT, as a tool cut short is asked), and kills
/// what is left of it once it has stopped or after [`GRACE`].
#[cfg(unix)]
async fn end_group(pid: u32) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let Some(group) = i32::try_from(pid).ok().filter(|p| *p > 1).map(Pid::from_raw) else {
        return;
    };
    if killpg(group, Signal::SIGINT).is_err() {
        return;
    }
    let deadline = tokio::time::Instant::now() + GRACE;
    // Signal 0: whether anything of the group is left.
    while killpg(group, None).is_ok() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(STOP_POLL).await;
    }
    if let Err(e) = killpg(group, Signal::SIGKILL) {
        tracing::debug!(pid, error = %e, "the tool's process group had ended");
    }
}

/// Ends process `pid` and its tree (`taskkill /T /F`): Windows has no signal to ask it first.
#[cfg(windows)]
#[expect(clippy::disallowed_methods, reason = "the process module's own ending of a group a killed operation left")]
async fn end_group(pid: u32) {
    let mut cmd = tokio::process::Command::new("taskkill");
    cmd.args(["/PID", &pid.to_string(), "/T", "/F"]).stdin(std::process::Stdio::null());
    headless(&mut cmd);
    if let Err(e) = cmd.output().await {
        tracing::warn!(pid, error = %e, "taskkill couldn't run to end a tool a killed operation left running");
    }
}

/// No console window for a helper this process runs.
#[cfg(windows)]
fn headless(cmd: &mut tokio::process::Command) {
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    cmd.creation_flags(CREATE_NO_WINDOW.0);
}

/// An entry of `dir` for the tool `pid`, recorded by a process that has died (an owner whose lock
/// nobody holds): what a killed operation leaves.
#[cfg(all(test, unix))]
pub(crate) fn left_by_a_killed_process(dir: &Path, pid: u32) -> io::Result<()> {
    crate::store::private_dir(dir)?;
    std::fs::write(dir.join("dead.owner"), "")?;
    let start = recorded_start(pid).ok_or_else(|| io::Error::other("no such process"))?;
    std::fs::write(dir.join(format!("dead-{pid}.tool")), format!("{pid} {}\n", start.entry_text()))
}

#[cfg(test)]
mod tests;
