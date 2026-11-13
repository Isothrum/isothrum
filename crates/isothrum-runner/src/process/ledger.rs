//! The tools an operation runs, kept in the run's state folder while they run, so the next
//! operation can end those a killed process left behind.
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
//! - `<owner>` is the recording process's token: that process holds `<owner>.owner` locked for as
//!   long as it lives, and the OS releases the lock when it dies, however it dies. An entry whose
//!   owner's lock can be taken is a dead process's.
//! - [`end_orphans`], with the run locked at the start of every operation, ends each tool a dead
//!   process left (its process group asked to stop, as a cut-short tool is, then killed; on
//!   Windows its tree, `taskkill /T /F`), and removes the entries. It ends only the very process
//!   that was recorded: one whose id now names another process (a later start, another program)
//!   is left alone, and so is a group whose leader is gone, which can't be told from another.

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
#[cfg(unix)]
use std::time::Duration;

/// The run's folder of tool entries, in its state folder.
pub(crate) const DIR: &str = "tools";

/// How far apart, in seconds, the start the OS stored and the one recorded may be for one
/// process (both read from the same clock at the same moment; the OS rounds to the second).
const SLACK: u64 = 2;

/// How long a tool left behind has to stop once asked (as a tool cut short has).
#[cfg(unix)]
const GRACE: Duration = super::stopping::GRACE;

/// A tool's start, as recorded and as the OS has it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Start {
    /// Linux: ticks since boot (`/proc/<pid>/stat`), and the boot's id.
    Boot { boot: String, ticks: u64 },
    /// Elsewhere: seconds since the epoch.
    At(u64),
}

impl Start {
    /// The entry's word for it.
    fn word(&self) -> String {
        match self {
            Start::Boot { boot, ticks } => format!("boot:{boot}:{ticks}"),
            Start::At(secs) => format!("at:{secs}"),
        }
    }

    /// [`Start::word`] read back.
    fn parse(word: &str) -> Option<Start> {
        if let Some(rest) = word.strip_prefix("boot:") {
            let (boot, ticks) = rest.rsplit_once(':')?;
            return Some(Start::Boot {
                boot: boot.to_owned(),
                ticks: ticks.parse().ok()?,
            });
        }
        word.strip_prefix("at:")?.parse().ok().map(Start::At)
    }

    /// Whether `now`, the start the OS has for the process, is this one.
    fn is(&self, now: &Start) -> bool {
        match (self, now) {
            (Start::At(a), Start::At(b)) => a.abs_diff(*b) <= SLACK,
            (a, b) => a == b,
        }
    }
}

/// One tool recorded: its entry, removed when this drops.
#[derive(Debug)]
pub(crate) struct Recorded(PathBuf);

impl Recorded {
    /// Records the tool `pid` just started in `dir`. A record that can't be written is traced:
    /// the tool runs unrecorded.
    pub(crate) fn write(dir: &Path, pid: u32) -> Option<Recorded> {
        let start = recorded_start(pid)?;
        let written = owner_token(dir).and_then(|token| {
            let path = dir.join(format!("{token}-{pid}.tool"));
            std::fs::write(&path, format!("{pid} {}\n", start.word()))?;
            Ok(path)
        });
        match written {
            Ok(path) => Some(Recorded(path)),
            Err(e) => {
                tracing::warn!(error = %e, dir = %dir.display(), "a tool couldn't be recorded: a killed operation would leave it running");
                None
            }
        }
    }
}

impl Drop for Recorded {
    fn drop(&mut self) {
        if let Err(e) = crate::store::if_there(std::fs::remove_file(&self.0)) {
            tracing::warn!(error = %e, path = %self.0.display(), "a tool's entry couldn't be removed");
        }
    }
}

/// The locks this process holds as the owner of each folder's entries, with its token.
/// Process-wide on purpose, where the runner keeps its other state per instance: an OS file lock
/// belongs to the process, not to a `Runner`, so two runners in one process are one owner and
/// must share one token per folder (a second token would read the first's entries as a dead
/// process's). The map's lock is held while an owner file is made, a few local file operations.
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

/// Whether the owner whose lock is `path` is gone: its lock can be taken (and is, until `path`
/// goes). A lock held by this process, or another live one, can't.
fn owner_gone(path: &Path) -> Option<File> {
    let file = File::options().read(true).write(true).open(path).ok()?;
    file.try_lock().ok().map(|()| file)
}

/// Ends each tool a dead process recorded in `dir` and removes its entries; the ids it ended.
pub(crate) async fn end_orphans(dir: &Path) -> Vec<u32> {
    let mut ended = Vec::new();
    let Ok(read) = std::fs::read_dir(dir) else { return ended };
    let names: Vec<String> = read.flatten().filter_map(|e| e.file_name().into_string().ok()).collect();
    for token in names.iter().filter_map(|n| n.strip_suffix(".owner")) {
        let owner = dir.join(format!("{token}.owner"));
        let Some(lock) = owner_gone(&owner) else { continue };
        let prefix = format!("{token}-");
        for entry in names
            .iter()
            .filter(|n| n.starts_with(&prefix) && Path::new(n).extension().is_some_and(|x| x == "tool"))
        {
            let path = dir.join(entry);
            if let Some(pid) = end_entry(&path).await {
                ended.push(pid);
            }
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

/// Ends the tool `path` records, when it is still the very process recorded: its id.
async fn end_entry(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let (pid, start) = text.trim().split_once(' ')?;
    let (pid, start): (u32, Start) = (pid.parse().ok()?, Start::parse(start)?);
    let now = os_start(pid).await;
    if !now.as_ref().is_some_and(|now| start.is(now)) {
        tracing::debug!(pid, "a tool a killed operation recorded is gone, or its id names another process: left alone");
        return None;
    }
    tracing::warn!(pid, "ending a tool a killed operation left running");
    end_group(pid).await;
    Some(pid)
}

/// The start to record for the tool `pid` that was just started.
fn recorded_start(pid: u32) -> Option<Start> {
    if cfg!(target_os = "linux") {
        return proc_start(pid);
    }
    Some(Start::At(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs()))
}

/// Process `pid`'s start on Linux: field 22 of `/proc/<pid>/stat` (after its name, which may
/// hold spaces and parentheses), and this boot's id.
fn proc_start(pid: u32) -> Option<Start> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    Some(Start::Boot {
        boot: boot.trim().to_owned(),
        ticks: stat_ticks(&stat)?,
    })
}

/// The start in a `/proc/<pid>/stat` line, in clock ticks since boot.
fn stat_ticks(stat: &str) -> Option<u64> {
    stat.rsplit_once(')')?.1.split_whitespace().nth(19)?.parse().ok()
}

/// Process `pid`'s start as the OS has it now; none when there is no such process.
async fn os_start(pid: u32) -> Option<Start> {
    if cfg!(target_os = "linux") {
        return proc_start(pid);
    }
    os_started_at(pid).await.map(Start::At)
}

/// When process `pid` started, in seconds since the epoch, as `ps` reads the start the kernel
/// stored (`lstart`, in UTC and the C locale's words).
#[cfg(unix)]
async fn os_started_at(pid: u32) -> Option<u64> {
    let out = tokio::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("TZ", "UTC")
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    out.status.success().then(|| lstart(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// When process `pid` started, as Windows stored its creation time (seconds since the epoch).
#[cfg(windows)]
async fn os_started_at(pid: u32) -> Option<u64> {
    let script = format!("[DateTimeOffset]::new((Get-Process -Id {pid} -ErrorAction Stop).StartTime).ToUnixTimeSeconds()");
    let mut cmd = tokio::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .stdin(std::process::Stdio::null());
    headless(&mut cmd);
    let out = cmd.output().await.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok()).flatten()
}

/// `ps`'s `lstart` (`Wed Nov 11 16:43:35 2026`) as seconds since the epoch.
#[cfg(any(unix, test))]
fn lstart(text: &str) -> Option<u64> {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let words: Vec<&str> = text.split_whitespace().collect();
    let [_, month, day, time, year] = words.as_slice() else { return None };
    let month = u64::try_from(MONTHS.iter().position(|m| m == month)? + 1).ok()?;
    let (day, year): (u64, u64) = (day.parse().ok()?, year.parse().ok()?);
    let mut hms = time.split(':').map(|p| p.parse::<u64>().ok());
    let (h, m, s) = (hms.next()??, hms.next()??, hms.next()??);
    Some(days_since_epoch(year, month, day)? * 86_400 + h * 3600 + m * 60 + s)
}

/// Days from 1970-01-01 to the date `year`-`month`-`day` (proleptic Gregorian, from 1970).
#[cfg(any(unix, test))]
fn days_since_epoch(year: u64, month: u64, day: u64) -> Option<u64> {
    let leap = |y: u64| (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let before_year: u64 = (1970..year).map(|y| if leap(y) { 366 } else { 365 }).sum();
    let lengths = [31, if leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let before_month: u64 = lengths.iter().take(usize::try_from(month.checked_sub(1)?).ok()?).sum();
    Some(before_year + before_month + day.checked_sub(1)?)
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if let Err(e) = killpg(group, Signal::SIGKILL) {
        tracing::debug!(pid, error = %e, "the tool's process group had ended");
    }
}

/// Ends process `pid` and its tree (`taskkill /T /F`): Windows has no signal to ask it first.
#[cfg(windows)]
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
    std::fs::write(dir.join(format!("dead-{pid}.tool")), format!("{pid} {}\n", start.word()))
}

#[cfg(test)]
#[path = "ledger_tests.rs"]
mod tests;
