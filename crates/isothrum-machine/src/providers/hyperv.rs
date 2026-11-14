//! Hyper-V's two ways of failing although the Windows feature is on: this account may not manage
//! it, and Windows' hypervisor isn't running.

use std::time::Duration;

use isothrum_runner::HyperVRights;

use super::Unavailable;
use crate::context::Context;
use crate::exec;

/// Whether this account may not manage Hyper-V, as the runner's check (`whoami /groups`)
/// answers: Vagrant's Hyper-V provider refuses every command (status reads included) from such an
/// account. When the check can't say, it may.
pub(super) async fn needs_admin(cx: &Context) -> bool {
    isothrum_runner::may_manage_hyperv(cx.runner.as_ref()).await == HyperVRights::Missing
}

/// How long Windows may take to say whether its hypervisor runs: WMI can hang on a damaged
/// repository, and an answer that doesn't come in time is no answer.
const HYPERVISOR_PROBE: Duration = Duration::from_secs(20);

/// The PowerShell that says whether Windows' hypervisor runs.
pub(super) const HYPERVISOR_PRESENT: &str = "(Get-CimInstance -ClassName Win32_ComputerSystem).HypervisorPresent";

/// Why an installed Hyper-V can't start a VM: Windows' hypervisor isn't running (virtualization off
/// in the firmware, or `hypervisorlaunchtype off`), which the feature's files and service don't
/// show. Asked of `Win32_ComputerSystem.HypervisorPresent`, which PowerShell prints as `True` or
/// `False` whatever the display language. `None` when it runs, or when Windows can't say (Hyper-V
/// is then taken as ready).
pub(super) async fn hypervisor_stopped(cx: &Context) -> Option<Unavailable> {
    let asked = exec::read_ok(
        cx.runner.as_ref(),
        HYPERVISOR_PROBE,
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", HYPERVISOR_PRESENT],
    )
    .await;
    (asked.as_deref().map(str::trim) == Ok("False")).then_some(Unavailable::HypervisorStopped)
}
