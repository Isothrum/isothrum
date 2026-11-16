//! What a Windows install's exit codes mean: winget's own result codes and the codes an installer
//! it ran exits with (a declined UAC prompt, an MSI its user backed out of), as codes a UI words.
//! The raw codes stay in the log.

/// winget's "the installer failed" code: what went wrong is the installer's own exit code, which
/// winget prints ("Installer failed with exit code: …").
const WINGET_INSTALLER_FAILED: u32 = 0x8A15_0006;
/// winget's "no applicable update": the package is installed already, at its latest version.
const WINGET_ALREADY_INSTALLED: u32 = 0x8A15_002B;
/// winget's "a restart is required to finish": the install (or removal) itself went through.
const WINGET_RESTART_TO_FINISH: u32 = 0x8A15_0109;

/// What a failed Windows step's codes say happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Known {
    /// Its user said no: to the UAC prompt, or to the installer.
    Cancelled,
    /// Another installation is already running.
    AnotherInstall,
    /// There isn't enough disk space.
    NoDiskSpace,
    /// It needs an internet connection.
    NoInternet,
    /// Windows must restart first: nothing was installed.
    RestartFirst,
    /// A policy on this computer blocks it.
    Policy,
    /// The installer reported an error: the details are in its output.
    InstallerError,
}

/// A winget result that isn't a failure although winget exits non-zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Done {
    /// Installed already, at its latest version: nothing to do.
    AlreadyInstalled,
    /// Done once Windows restarts.
    RestartToFinish,
}

/// What winget's own result codes mean (`APPINSTALLER_CLI_ERROR_*`, documented in winget-cli's
/// `doc/windows/package-manager/winget/returnCodes.md`).
fn winget_summary(code: u32) -> Option<Known> {
    Some(match code {
        0x8A15_010C => Known::Cancelled,
        0x8A15_0102 => Known::AnotherInstall,
        0x8A15_0105 => Known::NoDiskSpace,
        0x8A15_0107 => Known::NoInternet,
        0x8A15_010A => Known::RestartFirst,
        0x8A15_010F => Known::Policy,
        WINGET_INSTALLER_FAILED => Known::InstallerError,
        _ => return None,
    })
}

/// winget's results that are done although winget exits non-zero (a restart needed *before*
/// installing, `0x8A15010A`, is a failure: nothing was installed).
pub(crate) fn winget_done(code: Option<i32>) -> Option<Done> {
    match code?.cast_unsigned() {
        WINGET_ALREADY_INSTALLED => Some(Done::AlreadyInstalled),
        WINGET_RESTART_TO_FINISH => Some(Done::RestartToFinish),
        _ => None,
    }
}

/// Installer exit codes that mean its user said no (to the UAC prompt or the installer).
fn installer_cancelled(code: u32) -> bool {
    matches!(
        code,
        1223 // ERROR_CANCELLED: the UAC prompt was declined
            | 1602 // ERROR_INSTALL_USEREXIT: an MSI cancelled
            | 0x8007_04C7 // ERROR_CANCELLED as an HRESULT
            | 0xFFFF_FFFB // Docker Desktop's installer when its UAC prompt is declined
    )
}

/// The installer's exit code in a winget output line ("Installer failed with exit code: 1602").
/// Only winget's English output says it this way; in other languages winget's own code is what is
/// known.
pub(crate) fn installer_exit_code(line: &str) -> Option<u32> {
    let rest = line.trim().strip_prefix("Installer failed with exit code:")?.trim();
    match rest.strip_prefix("0x").or_else(|| rest.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => rest.parse::<u32>().ok().or_else(|| rest.parse::<i32>().ok().map(i32::cast_unsigned)),
    }
}

/// What a failed winget run's codes mean, when they are known.
pub(crate) fn winget_failure(code: Option<i32>, installer_code: Option<u32>) -> Option<Known> {
    let code = code?.cast_unsigned();
    match installer_code {
        Some(c) if code == WINGET_INSTALLER_FAILED && installer_cancelled(c) => Some(Known::Cancelled),
        _ => winget_summary(code),
    }
}

/// What a failed installer run through PowerShell means, from its own exit code: only a cancel is
/// known.
pub(crate) fn installer_failure(code: Option<i32>) -> Option<Known> {
    installer_cancelled(code?.cast_unsigned()).then_some(Known::Cancelled)
}

#[cfg(test)]
mod tests;
