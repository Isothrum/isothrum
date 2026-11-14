//! VirtualBox's own ways of failing although `VBoxManage` is installed: its kernel driver not
//! loaded (Linux), and its background service wedged.
//!
//! `VBoxManage --version` answers without the driver (after a warning), but no VM starts, so an
//! installed VirtualBox runs VMs only once `vboxdrv` is loaded. The module list says whether it
//! is, and whether a restart mends it: the driver is built when the computer starts a kernel whose
//! headers are installed (VirtualBox's `vboxdrv.sh start`, DKMS when the kernel is installed).
//! Where the module list can't be read, `VBoxManage`'s own warning says it.

use std::path::Path;
use std::time::Duration;

use super::Unavailable;
use crate::context::Context;
use crate::host::Os;

/// Why VirtualBox's kernel driver doesn't run here, `said` being what `VBoxManage --version`
/// printed: from the module list on Linux, else from its warning; `None` when it is loaded, or
/// warns of nothing.
pub(super) async fn driver_problem(cx: &Context, said: &str) -> Option<Unavailable> {
    let devices = &cx.host.devices;
    if cx.host.os == Os::Linux
        && let (Ok(modules), Ok(running)) = (std::fs::read_to_string(&devices.proc_modules), std::fs::read_to_string(&devices.kernel_release))
    {
        return from_modules(&modules, running.trim(), &devices.lib_modules);
    }
    if !driver_missing(said) {
        return None;
    }
    let kernel = cx.read_ok("uname", &["-r"]).await.map(|k| k.trim().to_owned()).unwrap_or_default();
    Some(Unavailable::VirtualboxDriver { kernel })
}

/// [`driver_problem`], from the text of `/proc/modules` (the modules loaded), the running kernel
/// (`uname -r`) and the kernels' module folders (`lib_modules/<kernel>`, whose `build` link is
/// there once that kernel's headers are installed): loaded, nothing; else a restart when the
/// running kernel has no headers and a newer kernel has (its install brought it); else the driver
/// to build or sign.
fn from_modules(modules: &str, running: &str, lib_modules: &Path) -> Option<Unavailable> {
    if modules.lines().any(|l| l.split_whitespace().next() == Some("vboxdrv")) {
        return None;
    }
    let has_headers = |kernel: &str| lib_modules.join(kernel).join("build").exists();
    if !has_headers(running) {
        let newer = std::fs::read_dir(lib_modules)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|k| has_headers(k) && release(k) > release(running))
            .max_by_key(|k| release(k));
        if let Some(kernel) = newer {
            return Some(Unavailable::KernelRestart { kernel });
        }
    }
    Some(Unavailable::VirtualboxDriver { kernel: running.to_owned() })
}

/// A kernel release's numbers, in order, up to its first letter: `6.12.116+deb13-amd64` is
/// 6.12.116, Ubuntu's `6.8.0-45-generic` 6.8.0-45 (its ABI number counts).
fn release(kernel: &str) -> Vec<u64> {
    let numbers = kernel.split(|c: char| c.is_ascii_alphabetic()).next().unwrap_or_default();
    numbers.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect()
}

/// Whether `said`, what `VBoxManage` printed, warns that its kernel driver isn't there: Oracle's
/// packages (`The vboxdrv kernel module is not loaded`) and the distributions'
/// (`The character device /dev/vboxdrv does not exist`).
fn driver_missing(said: &str) -> bool {
    said.lines().any(|l| l.trim_start().starts_with("WARNING:") && l.contains("vboxdrv"))
}

/// How long VirtualBox's service gets to answer before it counts as wedged, not just slow.
const SERVICE_WAIT: Duration = Duration::from_secs(8);

/// Whether VirtualBox's background service (`VBoxSVC`) is wedged: `VBoxManage --version` never
/// talks to it, so ask it something that does and see whether that times out.
///
/// A probe that times out is dropped, not awaited to its end: the process runner stops the hung
/// `VBoxManage` in the background (`SystemProcesses`: asked, then killed 30 s later), so it may
/// outlive this answer. An embedder that exits at once should await the runner's `stopped()`
/// first, as after any operation cut short.
pub(super) async fn wedged(cx: &Context) -> bool {
    let probe = crate::exec::probe(cx.runner.as_ref(), SERVICE_WAIT, "VBoxManage", &["list", "runningvms"]);
    matches!(probe.await, Err(crate::exec::Unanswered::TimedOut(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/lib/modules` with `kernels`, each with its headers (`build`) or not.
    fn lib_modules(kernels: &[(&str, bool)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (kernel, headers) in kernels {
            std::fs::create_dir_all(dir.path().join(kernel).join(if *headers { "build" } else { "kernel" })).unwrap();
        }
        dir
    }

    const LOADED: &str = "vboxnetflt 32768 0 - Live 0x0000000000000000 (OE)\nvboxdrv 696320 2 vboxnetadp,vboxnetflt, Live 0x0000000000000000 (OE)\n";
    const NOT_LOADED: &str = "kvm_intel 413696 0 - Live 0x0000000000000000\nkvm 1404928 1 kvm_intel, Live 0x0000000000000000\n";

    #[test]
    fn a_loaded_driver_is_no_problem() {
        let dir = lib_modules(&[("6.12.86+deb13-amd64", false)]);
        assert_eq!(from_modules(LOADED, "6.12.86+deb13-amd64", dir.path()), None);
    }

    #[test]
    fn a_driver_not_loaded_waits_for_the_newer_kernel_its_install_brought() {
        // Regression (a bare Debian 13): the archive had moved on to 6.12.116, so the running
        // 6.12.86 had no headers; VirtualBox read as ready, and a VM then found no driver.
        let dir = lib_modules(&[("6.12.86+deb13-amd64", false), ("6.12.116+deb13-amd64", true), ("6.12.94+deb13-amd64", true)]);
        assert_eq!(
            from_modules(NOT_LOADED, "6.12.86+deb13-amd64", dir.path()),
            Some(Unavailable::KernelRestart {
                kernel: "6.12.116+deb13-amd64".into()
            })
        );
        // Ubuntu's ABI number counts.
        let ubuntu = lib_modules(&[("6.8.0-45-generic", false), ("6.8.0-47-generic", true)]);
        assert_eq!(
            from_modules(NOT_LOADED, "6.8.0-45-generic", ubuntu.path()),
            Some(Unavailable::KernelRestart {
                kernel: "6.8.0-47-generic".into()
            })
        );
    }

    #[test]
    fn a_driver_not_loaded_with_nothing_a_restart_mends_is_to_build_or_sign() {
        // The running kernel's headers there (the build failed, or Secure Boot refused it).
        let built = lib_modules(&[("6.12.116+deb13-amd64", true)]);
        assert_eq!(
            from_modules(NOT_LOADED, "6.12.116+deb13-amd64", built.path()),
            Some(Unavailable::VirtualboxDriver {
                kernel: "6.12.116+deb13-amd64".into()
            })
        );
        // Only an older kernel's headers: a restart wouldn't help.
        let older = lib_modules(&[("6.12.86+deb13-amd64", true), ("6.12.116+deb13-amd64", false)]);
        assert_eq!(
            from_modules(NOT_LOADED, "6.12.116+deb13-amd64", older.path()),
            Some(Unavailable::VirtualboxDriver {
                kernel: "6.12.116+deb13-amd64".into()
            })
        );
    }

    #[test]
    fn kernel_releases_compare_by_their_numbers() {
        assert_eq!(release("6.12.116+deb13-amd64"), [6, 12, 116]);
        assert_eq!(release("6.8.0-45-generic"), [6, 8, 0, 45]);
        assert!(release("6.12.116+deb13-amd64") > release("6.12.86+deb13-amd64"));
    }

    /// Regression (#128): with its kernel driver not loaded, `VBoxManage` still answers, after a
    /// warning, and VirtualBox read as ready until `vagrant up` failed.
    #[test]
    fn vboxmanages_warning_names_the_missing_driver() {
        let oracle = "WARNING: The vboxdrv kernel module is not loaded. Either there is no module\n         available for the current kernel (6.8.0-49-generic) or it failed to\n         load.\n7.0.14r161095";
        let distribution = "WARNING: The character device /dev/vboxdrv does not exist.\n\t Please install the virtualbox-dkms package.\n7.0.16_Ubuntur162802";
        assert!(driver_missing(oracle) && driver_missing(distribution));
        assert!(!driver_missing("7.1.4r165100"));
        assert!(!driver_missing("WARNING: something else\n7.1.4"));
    }
}
