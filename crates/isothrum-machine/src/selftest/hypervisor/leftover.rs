//! The VM an earlier test left behind: one Vagrant no longer knows (its runner's state gone, say
//! with the caller's data folder) keeps running from the test folder, holds its disk, and the next
//! `vagrant up` fails on it ("the file is being used by another process" on Hyper-V, "a machine
//! with this name already exists" on VirtualBox). It is found by the test VM's name and removed,
//! but only when it is surely the test's: on Hyper-V every disk of it is under the test folder; on
//! VirtualBox its description names the test folder (set by the test's Vagrantfile). A VM of that
//! name that is anyone else's is left alone, and said.

use std::path::Path;

use isothrum_runner::{HostOs, ProcessSpec};

use super::{Failure, TestVm, vboxmanage};
use crate::Context;

/// What a removal found of a VM by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Leftover {
    /// No VM has the name.
    None,
    /// Every VM of the name was the test's: each was turned off and removed.
    Removed,
    /// A VM of the name isn't the test's: left alone.
    Foreign,
}

/// The exit codes of [`hyperv_leftover_script`]: what it found and did.
const HYPERV_REMOVED: i32 = 10;
const HYPERV_FOREIGN: i32 = 20;
/// Windows' code for a declined elevation prompt.
const ERROR_CANCELLED: i32 = 1223;

/// What a test VM's VirtualBox description says: it is the test's in `dir`.
pub(super) fn vbox_description(vm: &TestVm<'_>, dir: &Path) -> String {
    format!("{}:{}", vm.hostname, dir.display())
}

/// Removes a test VM an earlier run left on any hypervisor that can be asked by name: the
/// hypervisors it was removed from (none: nothing was left).
///
/// # Errors
///
/// A VM of the test's name that isn't the test's ([`Failure::Foreign`]), or a removal that
/// failed: the test can't boot its own VM until it is gone.
pub(super) async fn clear(cx: &Context, dir: &Path, vm: &TestVm<'_>) -> Result<Vec<&'static str>, Failure> {
    let mut removed = Vec::new();
    // Hyper-V asks for administrator rights: only when a VM disk is under the test folder.
    if cx.host.os == HostOs::Windows && holds_vhd(dir) {
        removed.extend(found("Hyper-V", remove_hyperv(cx, dir, vm).await?)?);
    }
    removed.extend(found("VirtualBox", remove_vbox(cx, dir, vm).await?)?);
    Ok(removed)
}

/// `hypervisor` when its leftover was removed; one of another's is [`Failure::Foreign`].
fn found(hypervisor: &'static str, leftover: Leftover) -> Result<Option<&'static str>, Failure> {
    match leftover {
        Leftover::None => Ok(None),
        Leftover::Removed => Ok(Some(hypervisor)),
        Leftover::Foreign => Err(Failure::Foreign { hypervisor }),
    }
}

/// Whether a Hyper-V disk (`.vhdx`) is anywhere under `dir`.
fn holds_vhd(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|entries| {
        entries.flatten().any(|e| {
            let path = e.path();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                holds_vhd(&path)
            } else {
                path.extension().is_some_and(|x| x.eq_ignore_ascii_case("vhdx"))
            }
        })
    })
}

/// [`clear`] on VirtualBox: its VMs of the test's exact name. None when VirtualBox isn't here.
async fn remove_vbox(cx: &Context, dir: &Path, vm: &TestVm<'_>) -> Result<Leftover, Failure> {
    let Ok(list) = cx.read_ok("VBoxManage", &["list", "vms"]).await else {
        return Ok(Leftover::None);
    };
    let ids: Vec<String> = isothrum_runner::virtualbox_vms(&list)
        .into_iter()
        .filter(|(name, _)| name == vm.name)
        .map(|(_, uuid)| uuid)
        .collect();
    if ids.is_empty() {
        return Ok(Leftover::None);
    }
    let ours = vbox_description(vm, dir);
    for id in &ids {
        let info = cx
            .read_ok("VBoxManage", &["showvminfo", id, "--machinereadable"])
            .await
            .map_err(Failure::Said)?;
        if vboxmanage::value(&info, "description").as_deref() != Some(ours.as_str()) {
            return Ok(Leftover::Foreign);
        }
    }
    for id in &ids {
        // A VM that isn't running refuses the power-off: only the removal must work.
        drop(cx.read_ok("VBoxManage", &["controlvm", id, "poweroff"]).await);
        cx.read_ok("VBoxManage", &["unregistervm", id, "--delete"]).await.map_err(Failure::Said)?;
    }
    Ok(Leftover::Removed)
}

/// `s` as a PowerShell single-quoted string; the typographic single quotes PowerShell also takes
/// as quotes doubled too.
fn ps_quote(s: &str) -> String {
    let doubled: String = s
        .chars()
        .flat_map(|c| std::iter::repeat_n(c, if matches!(c, '\'' | '\u{2018}'..='\u{201B}') { 2 } else { 1 }))
        .collect();
    format!("'{doubled}'")
}

/// `folder` as the `under` of [`hyperv_leftover_script`]: quoted, with one separator at its end.
fn folder_expr(folder: &Path) -> String {
    ps_quote(&format!("{}\\", folder.display().to_string().trim_end_matches(['\\', '/'])))
}

/// The PowerShell script that turns off and removes every Hyper-V VM named `name` whose disks are
/// all under the folder `under` (a PowerShell expression of its path, ending in a separator so a
/// sibling folder of the same prefix isn't under it), and nothing if any VM of that name has a
/// disk elsewhere (or none): it exits 0 (none), 10 (removed) or 20 (another's, left alone), so an
/// elevated run (whose output can't be read) says what it did too. An uninstaller can run it for
/// the test's folder.
#[must_use]
pub fn hyperv_leftover_script(name: &str, under: &str) -> String {
    format!(
        "$ErrorActionPreference = 'Stop'\n\
         $vms = @(Get-VM -Name {name} -ErrorAction SilentlyContinue | Where-Object {{ $_.Name -ceq {name} }})\n\
         if ($vms.Count -eq 0) {{ exit 0 }}\n\
         foreach ($vm in $vms) {{\n\
         \x20 $disks = @(Get-VMHardDiskDrive -VM $vm | ForEach-Object {{ $_.Path }})\n\
         \x20 $outside = @($disks | Where-Object {{ -not $_.StartsWith({under}, [System.StringComparison]::OrdinalIgnoreCase) }})\n\
         \x20 if ($disks.Count -eq 0 -or $outside.Count -gt 0) {{ exit {HYPERV_FOREIGN} }}\n\
         }}\n\
         foreach ($vm in $vms) {{ Stop-VM -VM $vm -TurnOff -Force; Remove-VM -VM $vm -Force }}\n\
         exit {HYPERV_REMOVED}\n",
        name = ps_quote(name),
    )
}

/// What [`hyperv_leftover_script`]'s exit `code` says.
fn hyperv_outcome(code: Option<i32>) -> Result<Leftover, Failure> {
    match code {
        Some(0) => Ok(Leftover::None),
        Some(HYPERV_REMOVED) => Ok(Leftover::Removed),
        Some(HYPERV_FOREIGN) => Ok(Leftover::Foreign),
        Some(ERROR_CANCELLED) => Err(Failure::Said(
            "the Windows permission prompt was declined, so the VM an earlier test left wasn't removed".to_owned(),
        )),
        other => Err(Failure::Said(format!(
            "removing the VM an earlier test left on Hyper-V failed (PowerShell exited with {})",
            other.map_or_else(|| "no code".to_owned(), |c| c.to_string())
        ))),
    }
}

/// [`clear`] on Hyper-V: [`hyperv_leftover_script`] run in an elevated Windows PowerShell (one prompt),
/// its exit code passed back through the PowerShell that started it.
async fn remove_hyperv(cx: &Context, dir: &Path, vm: &TestVm<'_>) -> Result<Leftover, Failure> {
    let inner = crate::wsl::encoded(&hyperv_leftover_script(vm.name, &folder_expr(dir)));
    let outer = format!(
        "try {{ $p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand','{inner}' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; exit $p.ExitCode }} catch {{ exit {ERROR_CANCELLED} }}"
    );
    let spec = ProcessSpec::new("powershell", ["-NoProfile", "-NonInteractive", "-Command", &outer]);
    let ran = cx
        .probe_within(super::super::PROMPT_LIMIT, &spec)
        .await
        .map_err(|e| Failure::Said(e.to_string()))?;
    hyperv_outcome(ran.exit.code)
}

#[cfg(test)]
mod tests;
