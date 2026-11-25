//! Letting VirtualBox share the CPU's virtualization with KVM (Linux,
//! [`super::Action::ShareVirtualization`]). Since Linux 6.12, KVM's module turns VT-x or AMD-V on
//! as it loads and keeps it, so no VirtualBox VM starts while KVM is loaded
//! ([`Unavailable::KvmHoldsVirtualization`]). With `options kvm enable_virt_at_load=0`, KVM takes
//! it only while one of its own VMs runs: both hypervisors are usable, just not at the same
//! moment.
//!
//! The option goes in a modprobe file of its own ([`OPTION_FILE`]), written as root the way an
//! install's steps are ([`Elevation`]). It applies the next time KVM's module loads: at once when
//! nothing uses KVM (no VM of its own running, nothing holding `/dev/kvm`), its modules unloaded
//! and loaded again; else from the next restart ([`Shared::AfterRestart`],
//! [`Unavailable::KvmSharesAfterRestart`]).

use crate::context::Context;
use crate::host::{Host, HostOs};
use crate::install::linux::Root;
use crate::install::root::RootScript;
use crate::install::{Elevation, Step, StepFailed, run_steps};
#[cfg(doc)]
use crate::providers::Unavailable;
use crate::providers::kvm;

/// The modprobe file the option is written to, in `/etc/modprobe.d`.
pub const OPTION_FILE: &str = "isothrum-kvm.conf";

/// The option itself: KVM turns the CPU's virtualization on only while one of its VMs runs.
pub const OPTION: &str = "options kvm enable_virt_at_load=0";

/// When VirtualBox can use the CPU's virtualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shared {
    /// Now: KVM doesn't hold it (it never did, or its modules were loaded again with the option).
    Now,
    /// Once the computer restarts: the option is written, but KVM was in use, or couldn't be
    /// loaded again ([`Unavailable::KvmSharesAfterRestart`]).
    AfterRestart,
}

/// Why KVM wasn't set to share the CPU's virtualization.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShareFailed {
    /// Not on Linux: KVM is Linux's.
    NotHere,
    /// KVM is built into the kernel, not a module modprobe loads: only the kernel's command line
    /// (`kvm.enable_virt_at_load=0`) sets its option.
    BuiltIn,
    /// The root step failed.
    Step(StepFailed),
    /// The step ran, yet KVM is neither sharing now nor set to from its next load.
    Unchanged,
}

impl std::fmt::Display for ShareFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShareFailed::NotHere => f.write_str("KVM runs on Linux only"),
            ShareFailed::BuiltIn => f.write_str("KVM is built into this kernel: start Linux with kvm.enable_virt_at_load=0"),
            ShareFailed::Step(failed) => failed.fmt(f),
            ShareFailed::Unchanged => f.write_str("KVM still holds the CPU's virtualization, and isn't set to share it"),
        }
    }
}

impl std::error::Error for ShareFailed {}

/// The root script that writes [`OPTION`] to [`OPTION_FILE`] in `modprobe_d`, then, when
/// `reload` names KVM's CPU module (`kvm_intel`, `kvm_amd`), unloads it and `kvm` and loads it
/// again, which loads `kvm` with the option. An unload modprobe refuses (a module in use) leaves
/// the option for the next restart; the CPU module is loaded again either way.
#[must_use]
pub fn share_script(modprobe_d: &std::path::Path, reload: Option<&str>) -> RootScript {
    let file = quoted(&modprobe_d.join(OPTION_FILE).to_string_lossy());
    let dir = quoted(&modprobe_d.to_string_lossy());
    let mut body = format!(
        "mkdir -p {dir}; printf '%s\\n' '# KVM takes the CPU'\\''s virtualization only while one of its VMs runs, so other hypervisors can use it.' '{OPTION}' > {file}"
    );
    if let Some(module) = reload {
        body = format!(
            "{body}; if modprobe -r {module} kvm; then echo 'KVM loaded again: it shares the CPU'\\''s virtualization now.'; \
             else echo 'KVM is in use: it shares the CPU'\\''s virtualization from the next restart.'; fi; modprobe {module}"
        );
    }
    RootScript::new(body)
}

/// `text` quoted for `sh`.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The step that sets KVM on `host` to share the CPU's virtualization, elevated by `elevation`:
/// its modules loaded again when nothing uses KVM now (`kvm::in_use`).
///
/// # Errors
///
/// [`ShareFailed::NotHere`] off Linux, [`ShareFailed::BuiltIn`] where KVM isn't a module.
pub fn share_step(host: &Host, elevation: Elevation) -> Result<Step, ShareFailed> {
    if host.os != HostOs::Linux {
        return Err(ShareFailed::NotHere);
    }
    if !kvm::loadable(host) {
        return Err(ShareFailed::BuiltIn);
    }
    let reload = kvm::cpu_module(host).filter(|_| !kvm::in_use(host));
    let root = Root { elevation, proxy: Vec::new() };
    // No proxy: nothing is downloaded, and no private file can fail to be written.
    root.step(share_script(&host.devices.modprobe_d, reload)).map_err(|_| ShareFailed::Unchanged)
}

/// When VirtualBox can use the CPU's virtualization on the context's host, as detection reads it
/// now: none when KVM holds it and isn't set to share it.
#[must_use]
pub fn sharing(host: &Host) -> Option<Shared> {
    if !kvm::holds_virtualization(host) {
        Some(Shared::Now)
    } else if kvm::shares_from_next_load(host) {
        Some(Shared::AfterRestart)
    } else {
        None
    }
}

/// Sets KVM to share the CPU's virtualization ([`share_step`], each line it prints handed to
/// `on_line`), then reads when VirtualBox can use it. Nothing runs where KVM doesn't hold it.
///
/// # Errors
///
/// [`ShareFailed`]: why it wasn't set.
pub async fn share_virtualization(cx: &Context, elevation: Elevation, on_line: &mut (dyn FnMut(&str) + Send)) -> Result<Shared, ShareFailed> {
    if cx.host.os != HostOs::Linux {
        return Err(ShareFailed::NotHere);
    }
    if !kvm::holds_virtualization(&cx.host) {
        return Ok(Shared::Now);
    }
    let step = share_step(&cx.host, elevation)?;
    run_steps(cx, &[step], on_line).await.map_err(ShareFailed::Step)?;
    sharing(&cx.host).ok_or(ShareFailed::Unchanged)
}

#[cfg(test)]
mod tests;
