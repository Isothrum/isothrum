//! The WSL 2 part of the system report (Windows only). The launcher probes WSL deeply — the
//! feature's state, whether a reboot is pending, whether virtualization is off in the firmware; that
//! full Windows probe is a follow-up port. For now [`report`] is `None` everywhere, which the setup
//! screen reads exactly as the "not applicable" it already shows on macOS and Linux.

use isothrum_runner::ProcessRunner;
use serde::Serialize;

use crate::dirs::Dirs;

/// Where WSL 2 stands on this machine.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "camelCase")]
pub enum WslState {
    /// On and running: a Docker engine can use it.
    Ready,
    /// Turned on (or being turned on), but Windows has to restart to finish.
    NeedsReboot,
    /// Not set up.
    Missing,
    /// The signals don't agree, or couldn't be read.
    Unknown,
}

/// The WSL part of the system report.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct WslReport {
    /// Where WSL 2 stands.
    pub state: WslState,
    /// Whether a restart reopens the app once the player signs back in.
    pub reopens_after_restart: bool,
    /// The Virtual Machine Platform is on but no hypervisor runs: virtualization is probably off in
    /// the firmware (the BIOS or UEFI setup).
    pub virtualization_off: bool,
}

/// The WSL report for this machine: `None` off Windows, and — until the full Windows probe is
/// ported from the launcher — `None` on Windows too. Takes the runner and dirs the ported probe
/// will need, so adding it is a drop-in change.
#[must_use]
pub fn report(_runner: &dyn ProcessRunner, _dirs: &Dirs) -> Option<WslReport> {
    None
}
