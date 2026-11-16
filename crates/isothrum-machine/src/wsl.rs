//! Whether WSL 2 is ready on a Windows computer.
//!
//! No single signal can be trusted: `wsl.exe` ships with Windows whether WSL is set up or not,
//! `wsl --version` only exists in the Store WSL, `wsl --status` may fail on a computer without a
//! distribution, and a feature turned on reads as on before the restart that really starts it.
//! So the check gathers several, none of which needs an administrator or reads localized text,
//! and [`classify`] (pure, tested on every OS) weighs them:
//! - `wsl --status` and `wsl --version`, by exit code only;
//! - the WSL services in the registry: `LxssManager` (the WSL feature) and `WslService` (the
//!   Store WSL);
//! - the optional features `VirtualMachinePlatform` and `Microsoft-Windows-Subsystem-Linux` (WMI
//!   `Win32_OptionalFeature`, readable without an administrator), and whether a hypervisor runs
//!   (`Win32_ComputerSystem.HypervisorPresent`: the Virtual Machine Platform only starts one after
//!   the restart);
//! - the restart Windows' servicing waits for (`Component Based Servicing\RebootPending`), and
//!   pending file renames, a weak sign (many installers, drivers and antivirus leave some at every
//!   boot), only heeded right after the caller's own install;
//! - what the caller did itself ([`Marks`]): whether its own "turn on WSL 2" ran since this boot,
//!   and whether it already restarted Windows for WSL (then a restart is never asked for again).
//!
//! Every probe runs through the [`Context`]'s runner, within a time limit: a hung `wsl.exe` costs
//! the check a few seconds, never the caller.

use std::time::Duration;

use isothrum_runner::ProcessSpec;
use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::exec::Unanswered;
use crate::host::HostOs;

/// Where WSL 2 stands on this computer.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum WslState {
    /// On and running.
    Ready,
    /// Turned on (or being turned on), but Windows has to restart to finish.
    NeedsReboot,
    /// Not set up.
    Missing,
    /// The signals don't agree, or couldn't be read.
    Unknown,
}

/// How a probe command went.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Run {
    /// Exited 0.
    Ok,
    /// Exited with another code (or couldn't be started for another reason than missing).
    Failed,
    /// Ran out of time and was ended.
    TimedOut,
    /// Not on this computer.
    Missing,
}

/// An optional feature's `Win32_OptionalFeature.InstallState`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feature {
    /// 1
    Enabled,
    /// 2
    Disabled,
    /// 3: not available on this edition, or not listed at all.
    Absent,
}

/// What the caller did itself, which no probe can read: it keeps them (by boot) and hands them
/// to [`probe`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Marks {
    /// The caller's own "turn on WSL 2" ran since this boot.
    pub installed_this_boot: bool,
    /// The caller restarted Windows for WSL in an earlier boot, with no install since.
    pub restarted_for_wsl: bool,
}

/// Every signal the check read. `None` is "couldn't tell" (unreadable, or the probe failed).
#[derive(Clone, Copy, Debug)]
pub struct Probes {
    /// `wsl.exe --status`.
    pub status: Run,
    /// `wsl.exe --version`: only the Store WSL knows this option.
    pub version: Run,
    /// The WSL feature's service (`LxssManager`) is registered.
    pub lxss_service: Option<bool>,
    /// The Store WSL's service (`WslService`) is registered.
    pub wsl_service: Option<bool>,
    /// The Virtual Machine Platform feature's state.
    pub vm_platform: Option<Feature>,
    /// The Windows Subsystem for Linux feature's state.
    pub wsl_feature: Option<Feature>,
    /// A hypervisor runs (what the Virtual Machine Platform starts at boot).
    pub hypervisor: Option<bool>,
    /// Windows' servicing waits for a restart.
    pub reboot_pending: Option<bool>,
    /// Files wait to be replaced at the next restart (weak: many installers leave some).
    pub renames_pending: Option<bool>,
    /// The caller's own "turn on WSL 2" ran since this boot ([`Marks`]).
    pub installed_this_boot: bool,
    /// The caller restarted Windows for WSL in an earlier boot ([`Marks`]).
    pub restarted_for_wsl: bool,
}

/// Where WSL 2 stands, from what the probes found.
#[must_use]
pub fn classify(p: &Probes) -> WslState {
    // WSL itself: the Store package, or the Windows feature.
    let store = p.version == Run::Ok || p.wsl_service == Some(true);
    let inbox = p.lxss_service == Some(true) || p.wsl_feature == Some(Feature::Enabled);
    let wsl = store || inbox;
    // WSL 2 runs its VM on the Virtual Machine Platform.
    let platform_on = p.vm_platform == Some(Feature::Enabled);
    let platform_off = matches!(p.vm_platform, Some(Feature::Disabled | Feature::Absent));
    let running = p.hypervisor == Some(true);
    let hypervisor_running = p.hypervisor != Some(false);
    let pending = p.reboot_pending == Some(true);

    // The caller's own install ran this boot and Windows waits for a restart: whatever the
    // features read meanwhile (they can read as on already), they only start after it. Renames
    // count here only: right after that install they are likely its own.
    if p.installed_this_boot && (pending || p.renames_pending == Some(true)) {
        return WslState::NeedsReboot;
    }
    if wsl && platform_on && running {
        // The platform is on and its hypervisor runs. A restart Windows waits for may be an
        // update's: then WSL has to answer too, or (without a distribution `wsl --status` fails)
        // be the Store WSL, which only installs once the platform is set up.
        if !pending || p.status == Run::Ok || (store && !p.installed_this_boot) {
            return WslState::Ready;
        }
    }
    // WSL answers and nothing waits for a restart; the features couldn't be read.
    let answers_with_features_unread = wsl && p.status == Run::Ok && p.reboot_pending == Some(false) && p.vm_platform.is_none();
    if answers_with_features_unread && hypervisor_running {
        return WslState::Ready;
    }
    if p.installed_this_boot {
        return WslState::NeedsReboot;
    }
    // A restart is pending and WSL is partly there: that restart finishes it. Never a second time
    // once the caller restarted for it: what is left isn't a restart's to fix.
    if (wsl || platform_on) && pending && !p.restarted_for_wsl {
        return WslState::NeedsReboot;
    }
    let wsl_failed = matches!(p.status, Run::Failed | Run::Missing);
    let no_services = p.lxss_service == Some(false) && p.wsl_service == Some(false);
    // Nothing of WSL, as both the services and wsl.exe say; or WSL 1 only, without the platform.
    if (!wsl && !platform_on && wsl_failed && (no_services || platform_off)) || (wsl && platform_off && !pending) {
        return WslState::Missing;
    }
    // Among the rest: on, yet no hypervisor (virtualization off in the firmware, most likely).
    WslState::Unknown
}

/// Whether virtualization looks off in the firmware: the platform is on, no hypervisor runs, and
/// no restart is still to come.
#[must_use]
pub fn virtualization_off(p: &Probes, state: WslState) -> bool {
    state == WslState::Unknown && p.vm_platform == Some(Feature::Enabled) && p.hypervisor == Some(false)
}

/// The features and the hypervisor, through WMI (no administrator needed), printed as
/// language-neutral `key=value` lines (see [`parse_cim`]). Each query fails on its own, and the
/// script exits 0 whatever happened, so what it printed is always read.
pub const CIM_SCRIPT: &str = "$ProgressPreference = 'SilentlyContinue'
try {
  $f = @(Get-CimInstance -ClassName Win32_OptionalFeature -Filter \"Name='VirtualMachinePlatform' OR Name='Microsoft-Windows-Subsystem-Linux'\" -ErrorAction Stop)
  'features=ok'
  foreach ($x in $f) { 'feature:' + $x.Name + '=' + [int]$x.InstallState }
} catch {}
try { 'hypervisor=' + [bool](Get-CimInstance -ClassName Win32_ComputerSystem -ErrorAction Stop).HypervisorPresent } catch {}
exit 0
";

/// The features and the hypervisor, from [`CIM_SCRIPT`]'s output: `features=ok` once the feature
/// query answered, then `feature:<name>=<InstallState>` for each feature it lists (an unlisted one
/// is absent), and `hypervisor=True|False`. Partial output reads as far as it goes.
#[must_use]
pub fn parse_cim(out: &str) -> (Option<Feature>, Option<Feature>, Option<bool>) {
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let answered = lines.contains(&"features=ok");
    let feature = |name: &str| {
        if !answered {
            return None;
        }
        let prefix = format!("feature:{name}=");
        match lines.iter().find_map(|l| l.strip_prefix(prefix.as_str())) {
            None | Some("3") => Some(Feature::Absent),
            Some("1") => Some(Feature::Enabled),
            Some("2") => Some(Feature::Disabled),
            Some(_) => None,
        }
    };
    let hypervisor = lines.iter().find_map(|l| l.strip_prefix("hypervisor=")).and_then(|v| match v {
        "True" => Some(true),
        "False" => Some(false),
        _ => None,
    });
    (feature("VirtualMachinePlatform"), feature("Microsoft-Windows-Subsystem-Linux"), hypervisor)
}

/// Where WSL 2 stands, and whether virtualization looks off in the firmware, with the signals
/// read (for a log).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Wsl {
    /// Where it stands ([`classify`]).
    pub state: WslState,
    /// Virtualization looks off in the firmware ([`virtualization_off`]).
    pub virtualization_off: bool,
    /// The signals it was decided on.
    pub probes: Probes,
}

/// How long `wsl.exe` may take.
const WSL_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the WMI queries may take.
const CIM_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a registry read may take.
const REG_TIMEOUT: Duration = Duration::from_secs(10);

/// The services' registry key.
const SERVICES: &str = r"HKLM\SYSTEM\CurrentControlSet\Services";

/// Where WSL 2 stands on the context's host, given what the caller did itself (`marks`): on
/// Windows only, `None` elsewhere.
pub async fn check(cx: &Context, marks: Marks) -> Option<Wsl> {
    if cx.host.os != HostOs::Windows {
        return None;
    }
    let probes = probe(cx, marks).await;
    let state = classify(&probes);
    Some(Wsl {
        state,
        virtualization_off: virtualization_off(&probes, state),
        probes,
    })
}

/// Every signal, the commands side by side. `wsl.exe` writes UTF-16 unless told otherwise: only
/// its exit codes are read, but its output in a log stays readable.
pub async fn probe(cx: &Context, marks: Marks) -> Probes {
    let wsl = |arg: &str| {
        let mut spec = ProcessSpec::new("wsl", [arg]);
        spec.env.push(("WSL_UTF8".to_owned(), "1".to_owned()));
        spec
    };
    let (status_spec, version_spec) = (wsl("--status"), wsl("--version"));
    let cim_spec = ProcessSpec::new("powershell", ["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded(CIM_SCRIPT)]);
    let (status, version, cim) = tokio::join!(
        cx.probe_within(WSL_TIMEOUT, &status_spec),
        cx.probe_within(WSL_TIMEOUT, &version_spec),
        cx.probe_within(CIM_TIMEOUT, &cim_spec),
    );
    let cim_out = match &cim {
        Ok(captured) if captured.success() => captured.stdout.join("\n"),
        _ => String::new(),
    };
    let (vm_platform, wsl_feature, hypervisor) = parse_cim(&cim_out);
    let lxss = format!(r"{SERVICES}\LxssManager");
    let store = format!(r"{SERVICES}\WslService");
    let servicing = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending";
    let session = r"HKLM\SYSTEM\CurrentControlSet\Control\Session Manager";
    let (lxss, store, servicing) = ([lxss.as_str()], [store.as_str()], [servicing]);
    let renames = [session, "/v", "PendingFileRenameOperations"];
    let (lxss_service, wsl_service, reboot_pending, renames_pending) =
        tokio::join!(registry(cx, &lxss), registry(cx, &store), registry(cx, &servicing), registry(cx, &renames),);
    Probes {
        status: outcome(&status),
        version: outcome(&version),
        lxss_service,
        wsl_service,
        vm_platform,
        wsl_feature,
        hypervisor,
        reboot_pending,
        renames_pending,
        installed_this_boot: marks.installed_this_boot,
        restarted_for_wsl: marks.restarted_for_wsl,
    }
}

/// How a timed probe went.
fn outcome(result: &Result<isothrum_runner::Captured, Unanswered>) -> Run {
    match result {
        Ok(captured) if captured.success() => Run::Ok,
        Err(Unanswered::NotStarted(isothrum_runner::ProcessError::Missing { .. })) => Run::Missing,
        Ok(_) | Err(Unanswered::NotStarted(_)) => Run::Failed,
        Err(Unanswered::TimedOut(_)) => Run::TimedOut,
    }
}

/// Whether a registry key (or, with `/v <name>`, a value) exists: `reg query` exits 0 when it
/// does and 1 when it doesn't; `None` when `reg` couldn't tell (it didn't start, or ran out of
/// time).
async fn registry(cx: &Context, query: &[&str]) -> Option<bool> {
    let spec = ProcessSpec::new("reg", std::iter::once("query").chain(query.iter().copied()));
    match cx.probe_within(REG_TIMEOUT, &spec).await {
        Ok(captured) => match captured.exit.code {
            Some(0) => Some(true),
            Some(1) => Some(false),
            _ => None,
        },
        Err(_) => None,
    }
}

/// A script for `powershell -EncodedCommand`: base64 of its UTF-16LE text (no quoting to get
/// wrong on the command line).
fn encoded(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0usize, |n, (i, b)| n | (usize::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            let sextet = ALPHABET.get((n >> (18 - 6 * i)) & 0x3f).copied().unwrap_or(b'=');
            out.push(char::from(if i <= chunk.len() { sextet } else { b'=' }));
        }
    }
    out
}

#[cfg(test)]
mod tests;
