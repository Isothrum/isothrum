//! What the Isothrum crates' tests share: [`ChildGuard`] and [`PidGuard`] end the helper
//! processes a test starts, whatever way the test ends, so none outlives it (or holds a build's
//! lock); [`wait_until`] waits for a condition with a deadline, never a fixed sleep; [`TempHome`]
//! is a home folder a spawned command is given instead of the real one; [`every`] and
//! [`assert_every!`] fail on an empty collection, where `iter().all(..)` passes; a program a test
//! runs is written by [`write_executable`] (Unix), never by the test's own process.
//!
//! A test-only crate: it panics where a test should fail.

#![expect(clippy::panic, reason = "a test-support helper fails the test that called it")]

use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// A spawned helper process, killed with its whole process group and reaped when dropped.
#[derive(Debug)]
pub struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    /// Spawns `command` in a process group of its own (on Unix), so what it starts goes with it.
    ///
    /// # Errors
    /// When it can't be spawned.
    pub fn spawn(command: &mut Command) -> std::io::Result<ChildGuard> {
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(command, 0);
        Ok(ChildGuard { child: Some(command.spawn()?) })
    }

    /// Its process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Waits for it to end and gives what it printed (when its output was piped); the guard is
    /// then spent.
    ///
    /// # Errors
    /// When waiting fails.
    pub fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        match self.child.take() {
            Some(child) => child.wait_with_output(),
            None => Err(std::io::Error::other("the child was already waited for")),
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            kill_group(child.id());
            // A test that didn't wait for it: it is ended now, and reaped.
            child.kill().ok();
            child.wait().ok();
        }
    }
}

/// A process a test learnt the id of (a grandchild: the tool a command started), killed with its
/// process group when dropped. It isn't the test's child, so its own parent reaps it.
#[derive(Debug)]
pub struct PidGuard(u32);

impl PidGuard {
    /// Guards process `pid`.
    #[must_use]
    pub fn new(pid: u32) -> PidGuard {
        PidGuard(pid)
    }

    /// Guards the process whose id `file` holds, once something wrote it.
    #[must_use]
    pub fn from_file(file: &Path) -> Option<PidGuard> {
        read_pid(file).map(PidGuard)
    }

    /// Its process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.0
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        kill_group(self.0);
        kill(self.0);
    }
}

/// Writes `contents` to `path` as an executable (on Unix) from a child process (`cat`), never
/// from the test's own: a file this process has open for writing is open in every process
/// another test thread forks meanwhile, until that one execs, and executing the file in that
/// window fails with "Text file busy" (ETXTBSY). A test never executes a file it wrote itself.
///
/// # Errors
/// When it can't be written.
#[cfg(unix)]
pub fn write_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut cat = Command::new("sh")
        .arg("-c")
        .arg("cat > \"$0\" && chmod 755 \"$0\"")
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    cat.stdin
        .take()
        .ok_or_else(|| std::io::Error::other("no stdin"))?
        .write_all(contents.as_bytes())?;
    let status = cat.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("writing {}: {status}", path.display())))
    }
}

/// The process id `file` holds, once it holds one.
#[must_use]
pub fn read_pid(file: &Path) -> Option<u32> {
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}

/// Kills the process group led by `pid`, if there is one.
#[cfg(unix)]
fn kill_group(pid: u32) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Ok(pid) = i32::try_from(pid) {
        killpg(Pid::from_raw(pid), Signal::SIGKILL).ok();
    }
}

#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// Kills process `pid`.
#[cfg(unix)]
fn kill(pid: u32) {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    if let Ok(pid) = i32::try_from(pid) {
        kill(Pid::from_raw(pid), Signal::SIGKILL).ok();
    }
}

#[cfg(not(unix))]
fn kill(pid: u32) {
    Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]).output().ok();
}

/// Whether process `pid` is there (a zombie counts as gone: in a container nothing may reap the
/// orphan it became).
#[must_use]
pub fn alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output();
        out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
    } else {
        let out = Command::new("ps").args(["-o", "stat=", "-p", &pid.to_string()]).output();
        out.is_ok_and(|o| {
            let stat = String::from_utf8_lossy(&o.stdout);
            !stat.trim().is_empty() && !stat.trim().starts_with('Z')
        })
    }
}

/// Polls `ready` every 20 ms until it holds or `deadline` passed: whether it held. For waiting on
/// real processes; code on Tokio's clock waits on events or the paused clock instead.
pub fn wait_until(deadline: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if ready() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A home folder for a spawned command, removed when dropped: [`TempHome::envs`] points `HOME`,
/// the XDG folders and Windows' profile folders into it, so nothing a test runs reads or writes
/// the real ones, and the test never changes its own process's environment.
#[derive(Debug)]
pub struct TempHome {
    dir: tempfile::TempDir,
}

impl TempHome {
    /// A new, empty one.
    ///
    /// # Panics
    /// When the temporary folder can't be made.
    #[must_use]
    pub fn new() -> TempHome {
        TempHome {
            dir: tempfile::tempdir().unwrap_or_else(|e| panic!("a temporary home: {e}")),
        }
    }

    /// Its path.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// The variables that make a command use it, with the locale fixed (`C`), so what the command
    /// prints is the same on every machine.
    #[must_use]
    pub fn envs(&self) -> Vec<(&'static str, std::path::PathBuf)> {
        let home = self.dir.path();
        vec![
            ("HOME", home.to_path_buf()),
            ("USERPROFILE", home.to_path_buf()),
            ("APPDATA", home.join("AppData/Roaming")),
            ("LOCALAPPDATA", home.join("AppData/Local")),
            ("XDG_CONFIG_HOME", home.join(".config")),
            ("XDG_DATA_HOME", home.join(".local/share")),
            ("XDG_STATE_HOME", home.join(".local/state")),
            ("XDG_CACHE_HOME", home.join(".cache")),
            ("LC_ALL", "C".into()),
            ("LANG", "C".into()),
        ]
    }
}

impl Default for TempHome {
    fn default() -> TempHome {
        TempHome::new()
    }
}

/// Whether `pred` holds for every item of `items`, failing the test when there is none:
/// `iter().all(..)` holds on nothing, so an assert over an empty list passes without checking.
///
/// # Panics
/// When `items` is empty.
pub fn every<T>(items: impl IntoIterator<Item = T>, mut pred: impl FnMut(&T) -> bool) -> bool {
    let mut seen = false;
    for item in items {
        seen = true;
        if !pred(&item) {
            return false;
        }
    }
    assert!(seen, "nothing to check: the collection is empty");
    true
}

/// Asserts that `$pred` holds for every item of `$items`, naming the first that doesn't, and that
/// there is at least one item.
#[macro_export]
macro_rules! assert_every {
    ($items:expr, $pred:expr $(,)?) => {{
        let items: Vec<_> = ::std::iter::IntoIterator::into_iter($items).collect();
        assert!(!items.is_empty(), "nothing to check: `{}` is empty", stringify!($items));
        let pred = $pred;
        for item in &items {
            assert!(pred(item), "`{}` doesn't hold for {:?}", stringify!($pred), item);
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "nothing to check")]
    fn every_fails_on_nothing() {
        every(Vec::<u8>::new(), |_| true);
    }

    #[test]
    fn every_holds_or_not_on_something() {
        assert!(every([1, 2], |&n| n > 0));
        assert!(!every([1, -2], |&n| n > 0));
    }

    #[test]
    #[should_panic(expected = "nothing to check")]
    fn assert_every_fails_on_nothing() {
        assert_every!(Vec::<u8>::new(), |_: &u8| true);
    }

    #[test]
    #[should_panic(expected = "doesn't hold for -2")]
    fn assert_every_names_the_first_item_that_fails() {
        assert_every!([1, -2, 3], |n: &i32| *n > 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_guarded_helper_and_what_it_started_end_with_the_guard() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("pid");
        let script = format!("sleep 300 & echo $! > '{}'; wait", file.display());
        let guard = ChildGuard::spawn(Command::new("sh").args(["-c", &script])).unwrap();
        let shell = guard.id();
        assert!(wait_until(Duration::from_secs(10), || read_pid(&file).is_some()));
        let sleeper = read_pid(&file).unwrap();
        assert!(alive(sleeper) && alive(shell));
        drop(guard);
        assert!(
            wait_until(Duration::from_secs(10), || !alive(sleeper)),
            "its child {sleeper} outlived the guard"
        );
        assert!(!alive(shell));
    }

    #[cfg(unix)]
    #[test]
    fn a_pid_guard_ends_a_process_the_test_didnt_start() {
        let mut child = Command::new("sleep").arg("300").spawn().unwrap();
        let guard = PidGuard::new(child.id());
        drop(guard);
        let status = child.wait().unwrap();
        assert!(!status.success(), "it was killed: {status:?}");
    }

    #[test]
    fn a_temp_home_points_every_home_variable_into_itself() {
        let home = TempHome::new();
        let envs = home.envs();
        assert_every!(envs.iter().filter(|(k, _)| !k.starts_with('L')), |(_, v): &&(&str, std::path::PathBuf)| v
            .starts_with(home.path()));
        assert!(envs.iter().any(|(k, v)| *k == "LC_ALL" && v.as_os_str() == "C"));
    }
}
