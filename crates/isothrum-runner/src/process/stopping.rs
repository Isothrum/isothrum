//! Tools cut short (their operation's future dropped: a cancel, Ctrl-C, a step's timeout) are
//! asked to stop before they are ended. On Unix the tool's process group gets SIGINT, which
//! Terraform, Vagrant and Docker Compose take as a request to stop cleanly (Terraform finishes
//! what it has in flight and saves its state), and SIGKILL once the tool has exited or after
//! [`GRACE`], whichever comes first, so nothing it started outlives it. Each one is waited for on
//! a thread of its own, since a drop can't wait: [`Stopping::stopped`] waits for them (a process
//! about to exit, such as the command line, calls it so none is left running), and
//! [`Stopping::end_now`] ends them at once (a second Ctrl-C).
//!
//! On Windows a tool runs headless, on a console of its own with no window, so the runner's own
//! Ctrl-C never reaches it. It is asked to stop with a Ctrl-C on that console, which Terraform, Ruby
//! (Vagrant) and Python take as SIGINT: the calls that send one are raw FFI, which this crate
//! forbids, so a short PowerShell helper makes them ([`ctrl_c_script`]: it attaches to the tool's
//! console, ignores the event itself, and sends it to every process there). Its job object is
//! terminated once it has exited or after the grace, as on Unix; a tool the helper can't reach
//! (its console gone, PowerShell missing) is terminated at once.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use process_wrap::tokio::ChildWrapper;

/// How long a tool asked to stop has before it is ended.
pub(super) const GRACE: Duration = Duration::from_secs(30);
/// How often a stopping tool is looked at.
const POLL: Duration = Duration::from_millis(50);

/// The tools cut short that are still stopping.
#[derive(Debug)]
pub(super) struct Stopping {
    /// How long each has.
    grace: Duration,
    /// How many are still stopping.
    count: AtomicUsize,
    /// Raised by [`Stopping::end_now`]: a tool asked to stop before it is ended at once.
    epoch: AtomicU64,
    /// Woken when a stopping tool ended (the count went down).
    done: tokio::sync::Notify,
}

/// A spawned tool, handed to the thread that stops it.
type Slot = Arc<Mutex<Option<Box<dyn ChildWrapper>>>>;

impl Stopping {
    /// Tools given `grace` to stop.
    pub(super) fn new(grace: Duration) -> Stopping {
        Stopping {
            grace,
            count: AtomicUsize::new(0),
            epoch: AtomicU64::new(0),
            done: tokio::sync::Notify::new(),
        }
    }

    /// Asks `child`'s tree to stop, and ends it once it has, or after the grace. Where it can't
    /// be asked (a tree already gone, a console the helper can't reach), it is ended at once. On
    /// Unix the signal goes now; on Windows asking takes a helper process, so it is asked on the
    /// thread that waits for it, never in the drop that cut it short.
    /// `removal` removes the container the tool runs in once the tool has ended: ending the
    /// `docker` client leaves its container running.
    pub(super) fn stop(self: &Arc<Self>, mut child: Box<dyn ChildWrapper>, recorded: Option<super::ledger::Recorded>, removal: Option<Removal>) {
        if cfg!(unix) && !interrupt(child.as_ref()) {
            end(child.as_mut());
            removal.inspect(Removal::run);
            return;
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        let since = self.epoch.load(Ordering::SeqCst);
        let slot: Slot = Arc::new(Mutex::new(Some(child)));
        let (stopping, theirs, their_removal) = (Arc::clone(self), Arc::clone(&slot), removal.clone());
        // Its entry in the run's tools goes once it has ended and its container is removed, not
        // before: this process killed meanwhile, the next operation ends it.
        let spawned = std::thread::Builder::new().name("isothrum-stop".into()).spawn(move || {
            if let Some(mut tool) = take(&theirs) {
                if cfg!(unix) || interrupt(tool.as_ref()) {
                    stopping.end_when_stopped(tool.as_mut(), since);
                } else {
                    end(tool.as_mut());
                }
            }
            their_removal.inspect(Removal::run);
            drop(recorded);
            stopping.count.fetch_sub(1, Ordering::SeqCst);
            stopping.done.notify_waiters();
        });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "no thread to wait for a tool to stop: it is ended at once");
            if let Some(mut tool) = take(&slot) {
                end(tool.as_mut());
            }
            removal.inspect(Removal::run);
            self.count.fetch_sub(1, Ordering::SeqCst);
            self.done.notify_waiters();
        }
    }

    /// Whether a tool is still stopping.
    pub(super) fn any(&self) -> bool {
        self.count.load(Ordering::SeqCst) > 0
    }

    /// Once no tool is stopping any more: woken by the last tool's end, not by polling. The wait
    /// is armed before the count is read, so an end between the two isn't missed.
    pub(super) async fn stopped(&self) {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.any() {
                return;
            }
            notified.await;
        }
    }

    /// Ends every tool still stopping, at once.
    pub(super) fn end_now(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }

    /// Waits for `child` to exit, for the grace at most and until [`Stopping::end_now`] (from
    /// epoch `since` on); then ends what is left of its tree.
    fn end_when_stopped(&self, child: &mut dyn ChildWrapper, since: u64) {
        let deadline = Instant::now() + self.grace;
        while Instant::now() < deadline && self.epoch.load(Ordering::SeqCst) == since && !exited(child) {
            std::thread::sleep(POLL);
        }
        end(child);
    }
}

/// The child in `slot`, taken out.
fn take(slot: &Slot) -> Option<Box<dyn ChildWrapper>> {
    slot.lock().unwrap_or_else(PoisonError::into_inner).take()
}

/// Whether the tool has exited (or can't be waited for any more).
fn exited(child: &mut dyn ChildWrapper) -> bool {
    !matches!(child.try_wait(), Ok(None))
}

/// SIGINT to the tool's process group; whether it was sent.
#[cfg(unix)]
fn interrupt(child: &dyn ChildWrapper) -> bool {
    tracing::debug!(pid = ?child.id(), "asking a process tree cut short to stop");
    match child.signal(nix::libc::SIGINT) {
        Ok(()) => true,
        Err(e) => {
            tracing::debug!(error = %e, "the process tree had already ended");
            false
        }
    }
}

/// How long the Windows helper that sends Ctrl-C may take (PowerShell compiles its few calls
/// first).
#[cfg(windows)]
const HELPER_LIMIT: Duration = Duration::from_secs(20);

/// A Ctrl-C on the console of the tool `child` (see the module's documentation); whether it was
/// sent.
#[cfg(windows)]
fn interrupt(child: &dyn ChildWrapper) -> bool {
    use std::os::windows::process::CommandExt as _;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    let Some(pid) = child.id() else { return false };
    tracing::debug!(pid, "asking a process tree cut short to stop (Ctrl-C on its console)");
    let helper = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &ctrl_c_script(pid)])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn();
    let mut helper = match helper {
        Ok(helper) => helper,
        Err(e) => {
            tracing::warn!(pid, error = %e, "PowerShell couldn't run to ask a tool to stop: it is ended at once");
            return false;
        }
    };
    let deadline = Instant::now() + HELPER_LIMIT;
    loop {
        match helper.try_wait() {
            Ok(Some(status)) if status.success() => return true,
            Ok(Some(status)) => {
                tracing::debug!(pid, code = ?status.code(), "the tool's console couldn't be sent Ctrl-C: it is ended at once");
                return false;
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) | Err(_) => {
                if let Err(e) = helper.kill() {
                    tracing::debug!(error = %e, "the Ctrl-C helper had ended");
                }
                tracing::warn!(pid, "the Ctrl-C helper didn't answer: the tool is ended at once");
                return false;
            }
        }
    }
}

/// The PowerShell that sends Ctrl-C to every process on process `pid`'s console: it leaves its
/// own console, attaches to the tool's, ignores Ctrl-C itself, and sends it to the console's
/// processes (group 0). Exit 0 once it was sent.
#[cfg(any(windows, test))]
fn ctrl_c_script(pid: u32) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         Add-Type -Namespace Isothrum -Name Ctrl -MemberDefinition @'\n\
         [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool FreeConsole();\n\
         [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool AttachConsole(uint pid);\n\
         [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool SetConsoleCtrlHandler(System.IntPtr handler, bool add);\n\
         [DllImport(\"kernel32.dll\", SetLastError = true)] public static extern bool GenerateConsoleCtrlEvent(uint ctrlEvent, uint group);\n\
         '@\n\
         [void][Isothrum.Ctrl]::FreeConsole()\n\
         if (-not [Isothrum.Ctrl]::AttachConsole({pid})) {{ exit 2 }}\n\
         [void][Isothrum.Ctrl]::SetConsoleCtrlHandler([System.IntPtr]::Zero, $true)\n\
         if (-not [Isothrum.Ctrl]::GenerateConsoleCtrlEvent(0, 0)) {{ exit 3 }}\n\
         exit 0\n"
    )
}

/// The removal of the container a tool runs in (`docker rm -f <name>`), run with the tool's own
/// environment so it reaches the same engine: ending the `docker` client leaves its container
/// running, and the tool in it.
#[derive(Debug, Clone)]
pub(super) struct Removal {
    /// The Docker CLI the tool ran.
    pub(super) program: String,
    /// The container's name.
    pub(super) name: String,
    /// The inherited variables the tool didn't get.
    pub(super) withheld: Vec<String>,
    /// The variables the tool was given.
    pub(super) env: Vec<(String, String)>,
    /// The engine overrides dropped after those (the user picked a context).
    pub(super) stripped: Vec<String>,
}

impl Removal {
    /// Removes the container, waiting for it. One its client removed already (it exited when
    /// asked), or no engine answering, is traced only.
    pub(super) fn run(&self) {
        let mut cmd = std::process::Command::new(&self.program);
        cmd.args(["rm", "-f", &self.name])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        for var in &self.withheld {
            cmd.env_remove(var);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        for var in &self.stripped {
            cmd.env_remove(var);
        }
        headless(&mut cmd);
        match cmd.status() {
            Ok(status) if status.success() => {
                tracing::debug!(container = %self.name, "removed the container of a tool that was ended");
            }
            Ok(status) => {
                tracing::debug!(container = %self.name, code = ?status.code(), "the container of a tool that was ended wasn't removed (gone already, or no engine)");
            }
            Err(e) => {
                tracing::warn!(container = %self.name, error = %e, "the container of a tool that was ended couldn't be removed: it may still run");
            }
        }
    }
}

/// No console window for a removal.
#[cfg(windows)]
fn headless(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    use windows::Win32::System::Threading::CREATE_NO_WINDOW;
    cmd.creation_flags(CREATE_NO_WINDOW.0);
}

/// Nothing to hide outside Windows.
#[cfg(not(windows))]
fn headless(_cmd: &mut std::process::Command) {}

/// SIGKILL to the process group, or the job object terminated: nothing the tool started outlives
/// it. Already gone is fine.
fn end(child: &mut dyn ChildWrapper) {
    tracing::debug!(pid = ?child.id(), "ending a process tree cut short");
    if let Err(e) = child.start_kill() {
        tracing::debug!(error = %e, "the process tree had already ended");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Windows helper attaches to the tool's own console and sends Ctrl-C (event 0) to every
    /// process there (group 0), having first made itself deaf to it.
    #[test]
    fn the_ctrl_c_helper_attaches_to_the_tools_console_and_ignores_the_event_itself() {
        let script = ctrl_c_script(4242);
        let at = |needle: &str| script.find(needle).unwrap_or_else(|| panic!("{needle} in {script}"));
        assert!(at("FreeConsole()\n") < at("AttachConsole(4242)"));
        assert!(at("AttachConsole(4242)") < at("SetConsoleCtrlHandler([System.IntPtr]::Zero, $true)"));
        assert!(at("SetConsoleCtrlHandler([System.IntPtr]::Zero, $true)") < at("GenerateConsoleCtrlEvent(0, 0)"));
        // A here-string's end must start its line.
        assert!(script.contains("\n'@\n"), "{script}");
    }
}
