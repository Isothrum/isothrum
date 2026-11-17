//! Whether a provider is ready for a start right now, beyond what detection reads: UTM open and
//! answering Apple events, and QEMU's system emulator for each CPU a project's machines are built
//! for. Detection ([`super::detect`]) says a hypervisor is installed; these are asked right before
//! a start, since they change while the computer runs (UTM quit, an Automation permission turned
//! off).
//!
//! UTM is opened hidden and in the background (`open -g -j`), never with its window in front,
//! and asked for its machines through `osascript`, as Vagrant's UTM plugin asks it. Every tool
//! runs through the [`Context`]'s runner.

use std::time::Duration;

use isothrum_runner::{ErrorCode, ProcessSpec, ProviderNeed};

use crate::context::Context;
use crate::exec;
use crate::host::{Arch, HostOs, qemu_system};

/// How long UTM may take to open and answer Apple events, by default.
pub const UTM_WAIT: Duration = Duration::from_secs(60);

/// How long one Apple event to UTM may take.
const UTM_ASK: Duration = Duration::from_secs(15);

/// The pause between two asks while UTM opens.
const UTM_RETRY: Duration = Duration::from_secs(1);

/// What UTM is asked, once it runs: its machines. Not its name: UTM answers that while it is
/// still loading, then refuses a machine's import a moment later with "UTM is not ready to accept
/// commands. (-2700)"; it lists its machines only once it takes commands.
const UTM_PROBE: &str = "tell application \"UTM\" to get id of every virtual machine";

/// The Apple event error macOS answers when the sender may not drive the app (its Automation
/// permission): no wait fixes it.
const NOT_AUTHORIZED: i32 = -1743;

/// Why a provider isn't ready for a start.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum NotReady {
    /// The system refuses what driving the hypervisor needs: UTM's Apple events
    /// ([`ProviderNeed::Automation`], `-1743`). Waiting doesn't fix it; the user's settings do.
    Needs(ProviderNeed),
    /// The hypervisor's app couldn't be opened (not installed where it was looked for).
    NotOpened {
        /// Why, as the system said it.
        said: String,
    },
    /// The hypervisor's app didn't answer within `waited`.
    NoAnswer {
        /// How long it was waited for.
        waited: Duration,
        /// Its last answer.
        said: String,
    },
    /// QEMU's system emulator for `arch` isn't installed.
    Emulator {
        /// The CPU it emulates.
        arch: Arch,
        /// Its program (`qemu-system-x86_64`).
        program: &'static str,
    },
}

impl NotReady {
    /// The runner's code for it: a refusal of the system's is [`ErrorCode::ProviderRefused`], a
    /// missing app or emulator [`ErrorCode::ToolMissing`], no answer [`ErrorCode::Timeout`].
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            NotReady::Needs(_) => ErrorCode::ProviderRefused,
            NotReady::NotOpened { .. } | NotReady::Emulator { .. } => ErrorCode::ToolMissing,
            NotReady::NoAnswer { .. } => ErrorCode::Timeout,
        }
    }
}

impl std::fmt::Display for NotReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotReady::Needs(ProviderNeed::Automation) => f.write_str("macOS doesn't allow this app to control UTM (Automation permission)"),
            NotReady::Needs(need) => write!(f, "the system refuses what the hypervisor needs ({need:?})"),
            NotReady::NotOpened { said } => write!(f, "UTM couldn't be opened: {said}"),
            NotReady::NoAnswer { waited, said } => write!(f, "UTM didn't answer within {} s: {said}", waited.as_secs()),
            NotReady::Emulator { program, .. } => write!(f, "QEMU's emulator {program} isn't installed"),
        }
    }
}

impl std::error::Error for NotReady {}

/// Opens UTM hidden and in the background (`open -g -j -a <UTM's bundle>`: `-g` alone still
/// shows its window over the caller's), then asks it for its machines until it answers, for at
/// most `wait` ([`UTM_WAIT`] is a start's). A UTM already open is left as it is. `waiting` is
/// called once, when it doesn't answer at first (UTM opening), so the caller can say so. Nothing
/// to do on any OS but macOS.
///
/// # Errors
///
/// [`NotReady::NotOpened`] when UTM can't be opened, [`NotReady::Needs`] with
/// [`ProviderNeed::Automation`] when macOS refuses this app Apple events to it (`-1743`), and
/// [`NotReady::NoAnswer`] when it doesn't answer within `wait`.
pub async fn utm(cx: &Context, wait: Duration, waiting: &mut (dyn FnMut() + Send)) -> Result<(), NotReady> {
    if cx.host.os != HostOs::Macos {
        return Ok(());
    }
    let app = cx.host.devices.utm_app.to_string_lossy().into_owned();
    let open = ProcessSpec::new("open", ["-g", "-j", "-a", app.as_str()]);
    match cx.probe_within(cx.probe_timeout, &open).await {
        Ok(opened) if opened.success() => {}
        Ok(failed) => return Err(NotReady::NotOpened { said: failed.reason() }),
        Err(e) => return Err(NotReady::NotOpened { said: e.to_string() }),
    }
    let ask = ProcessSpec::new("osascript", ["-e", UTM_PROBE]);
    let started = tokio::time::Instant::now();
    let mut said_waiting = false;
    loop {
        let said = match cx.probe_within(UTM_ASK, &ask).await {
            Ok(answer) if answer.success() => return Ok(()),
            Ok(refused) if apple_event(&exec::all_lines(&refused)) == Some(NOT_AUTHORIZED) => {
                return Err(NotReady::Needs(ProviderNeed::Automation));
            }
            Ok(refused) => refused.reason(),
            Err(e) => e.to_string(),
        };
        if started.elapsed() >= wait {
            return Err(NotReady::NoAnswer { waited: wait, said });
        }
        if !said_waiting {
            waiting();
            said_waiting = true;
        }
        tokio::time::sleep(UTM_RETRY).await;
    }
}

/// The CPUs of `machines` (a project's machines' CPUs) this computer emulates, each once, in
/// order: those other than its own. QEMU's system emulator for each must be installed
/// (`qemu-system-x86_64 --version` answers): an x86 machine on an Apple silicon Mac needs it.
///
/// # Errors
///
/// [`NotReady::Emulator`] naming the first CPU whose emulator isn't installed.
pub async fn emulators(cx: &Context, machines: impl IntoIterator<Item = Arch>) -> Result<Vec<Arch>, NotReady> {
    let mut emulated = Vec::new();
    for arch in machines {
        if arch == cx.host.arch || emulated.contains(&arch) {
            continue;
        }
        let program = qemu_system(arch);
        if cx.read_ok(program, &["--version"]).await.is_err() {
            return Err(NotReady::Emulator { arch, program });
        }
        emulated.push(arch);
    }
    Ok(emulated)
}

/// The last Apple event error number `said` names (`(-1743)`, ending `osascript`'s message in
/// every language); none when it names none.
fn apple_event(said: &str) -> Option<i32> {
    said.match_indices("(-")
        .filter_map(|(at, _)| {
            let rest = said.get(at + 1..)?;
            rest.get(..rest.find(')')?)?.parse::<i32>().ok()
        })
        .last()
}

#[cfg(test)]
mod tests;
