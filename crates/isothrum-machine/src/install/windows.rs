//! Installs and removals on Windows: winget's packages, WSL's own installer and Hyper-V's Windows
//! feature (DISM), each elevated through Windows' own UAC prompt, and HashiCorp's signed MSI for
//! VMware's utility. A declined prompt exits `ERROR_CANCELLED` (1223), which reads as cancelled
//! ([`super::exit_codes::Known::Cancelled`]).

use crate::host::{Arch, HostOs};
use crate::install::steps::{NotServed, Step, step};
use crate::install::{Dependency, Refusal, vmware_utility};
use crate::providers::{Provider, ProviderRow};

/// `winget <verb> <id>`, from the `winget` source alone. Without `--source`, winget asks every
/// source it has, the Microsoft Store's (`msstore`) too, and one that fails stops the command: on a
/// clean Windows 11 the Store source's certificate pinning failed (0x8a15005e, "The server
/// certificate did not match any of the expected values") and Docker Desktop's install with it.
/// Every package a setup installs is in the `winget` source.
#[must_use]
pub fn winget(verb: &str, id: &str, extra: &[&str]) -> Step {
    let mut args = vec![verb, "-e", "--id", id, "--source", "winget"];
    args.extend_from_slice(extra);
    step("winget", &args)
}

/// Turns WSL's and the Virtual Machine Platform's features on and installs the WSL 2 kernel,
/// through Windows' UAC prompt; the features take effect at the next restart. `-PassThru` surfaces
/// `wsl.exe`'s own exit code (without it PowerShell exits 0 whatever happened).
const WSL_ON: &str = "try { $p = Start-Process -FilePath wsl.exe -ArgumentList '--install','--no-distribution' -Verb RunAs -Wait -PassThru; exit $p.ExitCode } catch { exit 1223 }";

/// Turns Hyper-V on with Windows' own servicing tool (DISM), elevated: its "restart to finish"
/// (3010) is a success. On Windows Home, which has no Hyper-V, it stops before anything runs with
/// [`NOT_SERVED`](super::steps::NOT_SERVED) (69), for a UI to word.
const HYPERV_ON: &str = "if ((Get-ItemProperty 'HKLM:\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion').EditionID -like 'Core*') { exit 69 }; try { $p = Start-Process -FilePath dism.exe -ArgumentList '/Online','/Enable-Feature','/FeatureName:Microsoft-Hyper-V','/All','/NoRestart','/Quiet' -Verb RunAs -Wait -PassThru; if ($p.ExitCode -eq 3010) { exit 0 }; exit $p.ExitCode } catch { exit 1223 }";

/// Why `dep` isn't installed on a CPU `arch`: on Windows on Arm, the hypervisor it is or serves
/// (VMware's utility serves VMware Workstation) isn't built for it ([`ProviderRow::of`]).
fn not_on_arm(dep: Dependency, arch: Arch) -> Option<Refusal> {
    let provider = match dep {
        Dependency::VmwareUtility => Provider::VmwareDesktop,
        other => other.hypervisor()?,
    };
    (arch == Arch::Arm64 && !ProviderRow::of(provider).windows_on_arm).then_some(Refusal::NotOnWindowsOnArm { dependency: dep, provider })
}

/// The install of `dep` on Windows, on a CPU `arch`: its winget package, or WSL's, Hyper-V's and
/// VMware's utility's own elevated installers.
///
/// # Errors
///
/// [`Refusal`]: why it can't be installed here.
pub fn install(dep: Dependency, arch: Arch) -> Result<Vec<Step>, Refusal> {
    if !dep.runs_on(HostOs::Windows) {
        return Err(Refusal::NotHere {
            dependency: dep,
            os: HostOs::Windows,
        });
    }
    if let Some(refusal) = not_on_arm(dep, arch) {
        return Err(refusal);
    }
    if let Some(id) = dep.row().winget {
        return Ok(vec![winget("install", id, &["--accept-source-agreements", "--accept-package-agreements"])]);
    }
    match dep {
        Dependency::Wsl => Ok(vec![step("powershell", &["-NoProfile", "-Command", WSL_ON])]),
        Dependency::Hyperv => Ok(vec![
            step("powershell", &["-NoProfile", "-Command", HYPERV_ON]).not_served(NotServed::HypervEdition),
        ]),
        Dependency::VmwareUtility => Ok(vec![vmware_utility::windows_install()]),
        Dependency::Utm
        | Dependency::Libvirt
        | Dependency::Qemu
        | Dependency::Docker
        | Dependency::Vagrant
        | Dependency::Terraform
        | Dependency::Virtualbox
        | Dependency::Awscli
        | Dependency::Azurecli
        | Dependency::Gcloud => Err(Refusal::NotHere {
            dependency: dep,
            os: HostOs::Windows,
        }),
    }
}

/// The removal of `dep` on Windows: its winget package, VMware's utility by its MSI's product
/// code; the Windows features are their user's to turn off in Windows Features.
///
/// # Errors
///
/// [`Refusal::WindowsFeature`] for WSL and Hyper-V, [`Refusal::NotHere`] for what doesn't run on
/// Windows.
pub fn uninstall(dep: Dependency) -> Result<Vec<Step>, Refusal> {
    if let Some(id) = dep.row().winget {
        return Ok(vec![winget("uninstall", id, &[])]);
    }
    match dep {
        Dependency::VmwareUtility => Ok(vec![vmware_utility::windows_uninstall()]),
        Dependency::Wsl | Dependency::Hyperv => Err(Refusal::WindowsFeature { dependency: dep }),
        _ => Err(Refusal::NotHere {
            dependency: dep,
            os: HostOs::Windows,
        }),
    }
}

#[cfg(test)]
mod tests;
