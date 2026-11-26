//! The VirtualBox adapter: what a Vagrant run needs of `VBoxManage` by a VM's own id (its UUID,
//! the one Vagrant records): its state and remote display, discarding a saved state, powering it
//! off and deleting it.

use std::fmt;

use super::{Call, CliError, argv, said_any};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "VBoxManage";

/// A failed `VBoxManage` call, as far as a driver acts on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// VirtualBox has no VM of that id (`VBOX_E_OBJECT_NOT_FOUND`).
    NotFound,
    /// Any other failure: its reason.
    Other(String),
}

impl Failure {
    /// What a failed call that printed `out` failed with.
    pub(crate) fn of(out: &Captured) -> Failure {
        if said_any(out, &["vbox_e_object_not_found", "could not find a registered machine"]) {
            Failure::NotFound
        } else {
            Failure::Other(out.reason())
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Failure::NotFound => f.write_str("VirtualBox has no such VM"),
            Failure::Other(reason) => f.write_str(reason),
        }
    }
}

/// A VM's state, as `showvminfo --machinereadable` says it (`VMState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum VmState {
    /// Its saved state was cut off (a suspend killed midway, a host crash): VirtualBox refuses
    /// every change to it until the state is discarded.
    AbortedSaved,
    /// Any other state, as printed.
    Other(String),
}

/// The state `showvminfo --machinereadable` printed (`lines`); `None` when it prints none.
fn vm_state_of(lines: &[String]) -> Option<VmState> {
    let value = lines
        .iter()
        .find_map(|l| l.trim_end_matches('\r').strip_prefix("VMState="))
        .map(|v| v.trim().trim_matches('"'))?;
    Some(if value == "aborted-saved" {
        VmState::AbortedSaved
    } else {
        VmState::Other(value.to_owned())
    })
}

/// The state of VM `uuid`; [`Failure::NotFound`] when VirtualBox has none.
pub(crate) async fn vm_state(call: &Call<'_>, uuid: &str) -> Result<VmState, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["showvminfo", uuid, "--machinereadable"]), Failure::of).await?;
    vm_state_of(&out.stdout).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: format!("a `showvminfo` of `{uuid}` with no VMState"),
    })
}

/// Where VM `uuid`'s remote display (VRDE) listens, as `showvminfo --machinereadable` says;
/// [`Failure::NotFound`] when VirtualBox has no such VM.
pub(crate) async fn remote_display(call: &Call<'_>, uuid: &str) -> Result<crate::tool_output::vboxmanage::Vrde, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["showvminfo", uuid, "--machinereadable"]), Failure::of).await?;
    Ok(crate::tool_output::vboxmanage::remote_display(&out.stdout))
}

/// `discardstate <uuid>`: its saved state dropped, so it boots fresh from its disk.
pub(crate) async fn discard_state(call: &Call<'_>, uuid: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["discardstate", uuid]), Failure::of).await.map(drop)
}

/// `controlvm <uuid> poweroff`.
pub(crate) async fn power_off(call: &Call<'_>, uuid: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["controlvm", uuid, "poweroff"]), Failure::of).await.map(drop)
}

/// `unregistervm <uuid> --delete`: the VM and its files removed.
pub(crate) async fn unregister_delete(call: &Call<'_>, uuid: &str) -> Result<(), CliError<Failure>> {
    call.ok(TOOL, argv(["unregistervm", uuid, "--delete"]), Failure::of).await.map(drop)
}

/// Every VM VirtualBox has registered, as `(name, uuid)` (`list vms`).
pub(crate) async fn vms(call: &Call<'_>) -> Result<Vec<(String, String)>, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["list", "vms"]), Failure::of).await?;
    Ok(crate::tool_output::vboxmanage::vms(&out.stdout.join("\n")))
}

/// The folder VirtualBox makes each VM's folder in (`list systemproperties`' default machine
/// folder); [`CliError::Unexpected`] when it names none.
pub(crate) async fn machine_folder(call: &Call<'_>) -> Result<std::path::PathBuf, CliError<Failure>> {
    let out = call.ok(TOOL, argv(["list", "systemproperties"]), Failure::of).await?;
    crate::tool_output::vboxmanage::machine_folder(&out.stdout.join("\n")).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: "a `list systemproperties` with no default machine folder".to_owned(),
    })
}

#[cfg(test)]
mod tests;
