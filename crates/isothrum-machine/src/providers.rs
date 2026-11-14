//! The Vagrant providers (hypervisors) this computer can drive, and detecting which are usable.
//! Vagrant abstracts the hypervisor; we detect what is installed here and let `vagrant up
//! --provider` do the rest.
//!
//! Whether this account may manage Hyper-V is the runner's own check
//! ([`isothrum_runner::may_manage_hyperv`], `whoami /groups` through the same [`ProcessRunner`]),
//! the one it decides an elevated session by: when the account may not, Hyper-V's status says it
//! needs an administrator.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::{Arch, HostOs, ProcessRunner, this_arch, this_host, version_found};

use crate::exec;

/// A Vagrant provider (the value given to `vagrant up --provider`): the runner's.
pub use isothrum_runner::Provider;

/// Every provider the runner names, in its order: read from its type, so one added there is
/// detected without a list here to keep in step.
fn all() -> Vec<Provider> {
    isothrum_runner::every_name::<Provider>()
        .iter()
        .filter_map(|name| isothrum_runner::from_wire_name(name))
        .collect()
}

/// What detection reads of this computer besides its tools: its OS and CPU, KVM's device, UTM's
/// app bundle and the Hyper-V feature. Tests give their own, so a test takes the same path on
/// every machine.
#[derive(Debug, Clone)]
struct Host {
    /// Its OS.
    os: HostOs,
    /// Its CPU.
    arch: Arch,
    /// KVM's device.
    kvm: std::path::PathBuf,
    /// Whether UTM's app bundle is installed.
    utm_installed: bool,
    /// Whether the Hyper-V feature is on.
    hyperv_enabled: bool,
}

impl Host {
    /// This computer.
    fn this() -> Host {
        Host {
            os: this_host(),
            arch: this_arch(),
            kvm: KVM.into(),
            utm_installed: Path::new(UTM_APP).exists(),
            hyperv_enabled: hyperv_enabled(),
        }
    }

    /// Whether `provider` can run on it at all ([`applicable_on`]).
    fn applies(&self, provider: Provider) -> bool {
        applicable_on(provider, self.os, self.arch)
    }
}

/// How this computer tells a provider's hypervisor is installed.
#[derive(Debug, Clone, Copy)]
enum Check {
    /// A tool that proves it: installed as long as it starts.
    Tool(&'static str, &'static [&'static str]),
    /// QEMU's system emulator for this machine's CPU ([`qemu_binary_on`]).
    Qemu,
    /// UTM's app bundle, which also says its version.
    UtmApp,
    /// The Hyper-V Windows feature.
    HypervFeature,
    /// Nothing local to check: a remote hypervisor.
    Remote,
}

/// What detection knows of one provider, one row each ([`facts`]): a provider the runner adds is
/// one row here, not an arm in each check.
#[derive(Debug, Clone, Copy)]
struct Facts {
    /// The OSes it runs on; empty for any.
    oses: &'static [HostOs],
    /// Whether it runs on Windows on Arm (VirtualBox and VMware Workstation don't).
    windows_arm: bool,
    /// How its hypervisor is found.
    check: Check,
    /// The oldest version of its plugin that works: `vagrant_utm` before 0.2.0 reads a VM's
    /// forwarded ports through a script UTM refuses (`-600`, `-1743`) and has no `started`
    /// state.
    plugin_minimum: Option<&'static str>,
    /// Its hypervisor's name and the oldest version its plugin drives: `vagrant_utm` refuses a
    /// UTM before 4.6.5 (no import of a box, no registry for its shared folders).
    hypervisor_minimum: Option<(&'static str, &'static str)>,
}

/// A remote provider's facts: it applies anywhere, with nothing local to check.
const REMOTE: Facts = Facts {
    oses: &[],
    windows_arm: true,
    check: Check::Remote,
    plugin_minimum: None,
    hypervisor_minimum: None,
};

/// A local hypervisor's facts: it runs on `oses` (any, when empty), found by `check`.
const fn local(oses: &'static [HostOs], check: Check) -> Facts {
    Facts { oses, check, ..REMOTE }
}

/// What detection knows of `provider`: a provider the runner adds fails to compile here until it
/// has its row.
fn facts(provider: Provider) -> Facts {
    match provider {
        Provider::Virtualbox => Facts {
            windows_arm: false,
            ..local(&[], Check::Tool("VBoxManage", &["--version"]))
        },
        Provider::VmwareDesktop => Facts {
            windows_arm: false,
            ..local(&[], Check::Tool("vmrun", &[]))
        },
        Provider::Hyperv => local(&[HostOs::Windows], Check::HypervFeature),
        Provider::Parallels => local(&[HostOs::Macos], Check::Tool("prlctl", &["--version"])),
        Provider::Utm => Facts {
            plugin_minimum: Some("0.2.0"),
            hypervisor_minimum: Some(("UTM", "4.6.5")),
            ..local(&[HostOs::Macos], Check::UtmApp)
        },
        Provider::Libvirt => local(&[HostOs::Linux], Check::Tool("virsh", &["--version"])),
        Provider::Qemu => local(&[], Check::Qemu),
        Provider::VmwareEsxi => REMOTE,
    }
}

/// Whether `provider` can run on OS `os` and CPU `arch` (the runner's [`this_host`] and
/// [`this_arch`]): local hypervisors are platform-specific, remote ones apply anywhere.
fn applicable_on(provider: Provider, os: HostOs, arch: Arch) -> bool {
    let f = facts(provider);
    let windows_arm = os == HostOs::Windows && arch == Arch::Arm64;
    (f.oses.is_empty() || f.oses.contains(&os)) && (f.windows_arm || !windows_arm)
}

/// The hypervisor's own version, when it is installed and says it: UTM's app bundle
/// (`CFBundleShortVersionString`); the others aren't held to a version yet.
async fn hypervisor_version(provider: Provider, runner: &dyn ProcessRunner) -> Option<String> {
    if !matches!(facts(provider).check, Check::UtmApp) {
        return None;
    }
    let plist = format!("{UTM_APP}/Contents/Info.plist");
    let out = exec::read_any_exit(runner, "defaults", &["read", &plist, "CFBundleShortVersionString"])
        .await
        .ok()?;
    let version = out.trim().to_owned();
    (!version.is_empty()).then_some(version)
}

/// The local tool that proves the hypervisor itself is installed on a CPU `arch`, if there is one.
fn probe(provider: Provider, arch: Arch) -> Option<(&'static str, &'static [&'static str])> {
    match facts(provider).check {
        Check::Tool(program, args) => Some((program, args)),
        Check::Qemu => Some((qemu_binary_on(arch), &["--version"])),
        Check::UtmApp | Check::HypervFeature | Check::Remote => None,
    }
}

/// Whether the hypervisor itself is installed here, `None` when there is nothing local to
/// check (remote providers). UTM is an app bundle; Hyper-V is a Windows feature.
async fn hypervisor_installed(provider: Provider, host: &Host, runner: &dyn ProcessRunner) -> Option<bool> {
    match facts(provider).check {
        Check::UtmApp => Some(host.utm_installed),
        Check::HypervFeature => Some(host.hyperv_enabled),
        Check::Tool(..) | Check::Qemu | Check::Remote => {
            let (program, args) = probe(provider, host.arch)?;
            Some(tool_starts(runner, program, args).await)
        }
    }
}

/// Where UTM is installed on macOS.
const UTM_APP: &str = "/Applications/UTM.app";

/// Whether the Hyper-V feature is on: its Virtual Machine Management service is installed with it.
fn hyperv_enabled() -> bool {
    let root = std::env::var_os("SystemRoot").map_or_else(|| std::path::PathBuf::from("C:\\Windows"), std::path::PathBuf::from);
    root.join("System32").join("vmms.exe").is_file()
}

/// Why an installed Hyper-V can't start a VM: Windows' hypervisor isn't running (virtualization
/// off in the firmware, or `hypervisorlaunchtype off`, often set so VirtualBox runs faster), which
/// the feature's files and service don't show. Asked of `Win32_ComputerSystem.HypervisorPresent`,
/// which PowerShell prints as `True` or `False` whatever the display language. `None` when it runs,
/// or when Windows can't say.
async fn hypervisor_stopped(runner: &dyn ProcessRunner) -> Option<String> {
    // WMI can hang on a damaged repository: an answer that doesn't come in time is no answer.
    let asking = exec::read_ok(runner, "powershell", &["-NoProfile", "-NonInteractive", "-Command", HYPERVISOR_PRESENT]);
    let asked = tokio::time::timeout(HYPERVISOR_PROBE, asking).await.ok()?;
    // Any other answer, or none, leaves it unknown: Hyper-V is then taken as ready, as before.
    (asked.as_deref().map(str::trim) == Ok("False")).then(|| HYPERVISOR_STOPPED.to_owned())
}

/// How long Windows may take to say whether its hypervisor runs.
const HYPERVISOR_PROBE: Duration = Duration::from_secs(20);

/// The PowerShell that says whether Windows' hypervisor runs.
const HYPERVISOR_PRESENT: &str = "(Get-CimInstance -ClassName Win32_ComputerSystem).HypervisorPresent";

/// What an installed Hyper-V whose hypervisor doesn't run needs.
const HYPERVISOR_STOPPED: &str = "Hyper-V is turned on but Windows' hypervisor isn't running: turn virtualization on in the firmware, \
or run `bcdedit /set hypervisorlaunchtype auto` as an administrator, then restart Windows";

/// KVM's device, which vagrant-libvirt's guests run on.
const KVM: &str = "/dev/kvm";

/// Why libvirt can't run VMs here although `virsh` is installed: vagrant-libvirt boots KVM guests,
/// so it needs KVM's device `dev`, and the user must be allowed to open it. `None` when it is
/// fine. It reads the file system: [`detect`] asks it, [`unavailable_reason`] reads the answer.
/// A device the user may not open says how to get leave; any other failure is said as it is.
fn kvm_problem(dev: &Path) -> Option<String> {
    if !dev.exists() {
        return Some("KVM isn't available on this machine (no /dev/kvm): turn on virtualization in the firmware, or use another hypervisor".to_owned());
    }
    match std::fs::OpenOptions::new().read(true).write(true).open(dev) {
        Ok(_) => None,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            Some("you can't use /dev/kvm yet: add yourself to the kvm and libvirt groups, then sign out and back in".to_owned())
        }
        Err(e) => Some(format!("/dev/kvm can't be opened: {e}")),
    }
}

/// Why VirtualBox can't start a VM although `VBoxManage` runs: on Linux, its kernel driver isn't
/// loaded (a kernel updated since its module was built, Secure Boot refusing the unsigned module),
/// which VirtualBox's wrapper only warns of before running the command, whose answer is otherwise
/// as usual. `None` when it warns of nothing.
async fn vbox_driver_problem(runner: &dyn ProcessRunner) -> Option<String> {
    let said = exec::read_all(runner, "VBoxManage", &["--version"]).await.ok()?;
    vbox_driver_missing(&said).then(|| VBOX_DRIVER_MISSING.to_owned())
}

/// What VirtualBox whose kernel driver isn't loaded needs.
const VBOX_DRIVER_MISSING: &str = "VirtualBox's kernel driver isn't loaded, so it can't start VMs: rebuild it with `sudo /sbin/vboxconfig` \
(with Secure Boot on, enroll the key it signs it with), or reinstall VirtualBox for this kernel";

/// Whether `said`, what `VBoxManage` printed, warns that its kernel driver isn't there: Oracle's
/// packages (`The vboxdrv kernel module is not loaded`) and the distributions'
/// (`The character device /dev/vboxdrv does not exist`).
fn vbox_driver_missing(said: &str) -> bool {
    said.lines().any(|l| l.trim_start().starts_with("WARNING:") && l.contains("vboxdrv"))
}

/// The QEMU binary for a CPU `arch`.
fn qemu_binary_on(arch: Arch) -> &'static str {
    match arch {
        Arch::Arm64 => "qemu-system-aarch64",
        Arch::Amd64 => "qemu-system-x86_64",
    }
}

/// One provider's readiness on this machine.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProviderStatus {
    /// The provider.
    pub provider: Provider,
    /// Why it is not ready to use here, `None` when it is.
    pub reason: Option<String>,
}

/// The version `installed` of `provider`'s plugin when it is older than the one that works
/// ([`Facts::plugin_minimum`]), with that minimum; `None` when it is recent enough, unknown, or the
/// plugin has no minimum.
fn plugin_too_old(provider: Provider, installed: Option<&str>) -> Option<(&str, &'static str)> {
    let (version, min) = installed.zip(facts(provider).plugin_minimum)?;
    older(version, min).then_some((version, min))
}

/// The version `installed` of `provider`'s hypervisor when it is older than the one its plugin
/// drives ([`Facts::hypervisor_minimum`]), with the hypervisor's name and that minimum.
fn hypervisor_too_old(provider: Provider, installed: Option<&str>) -> Option<(&'static str, &str, &'static str)> {
    let (version, (name, min)) = installed.zip(facts(provider).hypervisor_minimum)?;
    older(version, min).then_some((name, version, min))
}

/// Whether the version `installed` says is before `min`, in Isoloom's order (a pre-release before
/// its release); not when either says no version.
fn older(installed: &str, min: &str) -> bool {
    version_found(installed).zip(version_found(min)).is_some_and(|(found, min)| found < min)
}

/// Whether `program` is installed: it is, as long as it could be started (a tool that prints usage
/// and exits non-zero, like `vmrun`, still exists).
async fn tool_starts(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> bool {
    exec::read_any_exit(runner, program, args).await.is_ok()
}

/// The Vagrant plugins installed, each with its version when it says one
/// ([`isothrum_runner::vagrant_plugins`]); why not when `vagrant plugin list` doesn't answer.
async fn installed_plugins(runner: &dyn ProcessRunner) -> Result<Vec<(String, Option<String>)>, String> {
    exec::read_ok(runner, "vagrant", &["plugin", "list", "--machine-readable"])
        .await
        .map(|out| isothrum_runner::vagrant_plugins(&out))
}

/// Every provider's reason without Vagrant.
const NO_VAGRANT: &str = "Vagrant is not installed";

/// How long the VirtualBox liveness probe may take before VirtualBox counts as wedged.
const VBOX_PROBE: Duration = Duration::from_secs(8);

/// Why Hyper-V can't be used by this account yet, and the two ways out. Folded into Hyper-V's
/// status when the account may not manage it ([`needs_admin`]).
const HYPERV_NEEDS_ADMIN: &str = "Vagrant drives Hyper-V only for an administrator or a member of the Hyper-V Administrators group: \
add this account to that group and sign out of Windows and back in, or run elevated";

/// What keeps `provider`'s installed hypervisor (`hypervisor`) from starting a VM on `host`:
/// KVM's device for libvirt, VirtualBox's kernel driver, Windows' hypervisor for Hyper-V. `None`
/// when nothing does.
async fn platform_problem(provider: Provider, hypervisor: Option<bool>, host: &Host, runner: &dyn ProcessRunner) -> Option<String> {
    match provider {
        Provider::Libvirt => kvm_problem(&host.kvm),
        Provider::Virtualbox if hypervisor == Some(true) => vbox_driver_problem(runner).await,
        Provider::Hyperv if hypervisor == Some(true) => hypervisor_stopped(runner).await,
        _ => None,
    }
}

/// The readiness of every provider that applies to this computer's OS and CPU, usable or not:
/// `None` when it is ready, else why not (its hypervisor, its plugin, Hyper-V needing an
/// administrator...). Without Vagrant (`vagrant_installed`, the caller's to know) each says so,
/// and nothing is probed.
pub async fn detect(runner: &dyn ProcessRunner, vagrant_installed: bool) -> Vec<ProviderStatus> {
    detect_on(&Host::this(), runner, vagrant_installed).await
}

/// [`detect`] on `host`.
async fn detect_on(host: &Host, runner: &dyn ProcessRunner, vagrant_installed: bool) -> Vec<ProviderStatus> {
    if !vagrant_installed {
        return all()
            .into_iter()
            .filter(|p| host.applies(*p))
            .map(|provider| ProviderStatus {
                provider,
                reason: Some(NO_VAGRANT.to_owned()),
            })
            .collect();
    }
    let plugins = installed_plugins(runner).await;
    // Vagrant that can't list its plugins says nothing of them: one reason for every provider
    // that needs one, rather than each plugin "not installed".
    let (plugins, plugins_unknown) = match plugins {
        Ok(plugins) => (plugins, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    let mut statuses = Vec::new();
    for provider in all() {
        if !host.applies(provider) {
            continue;
        }
        let hypervisor = hypervisor_installed(provider, host, runner).await;
        let hypervisor_version = if hypervisor == Some(true) {
            hypervisor_version(provider, runner).await
        } else {
            None
        };
        let plugin = provider.plugin();
        let installed = plugin.and_then(|needed| plugins.iter().find(|(i, _)| i == needed));
        let plugin_installed = plugin.is_none() || installed.is_some();
        let plugin_version = installed.and_then(|(_, v)| v.as_deref());
        let unresponsive = provider == Provider::Virtualbox && hypervisor == Some(true) && vbox_wedged(runner).await;
        let needs_admin = needs_admin(provider, hypervisor, runner).await;
        let platform_problem = platform_problem(provider, hypervisor, host, runner).await;
        let platform_problem = platform_problem.as_deref();
        let detection = Detection {
            arch: host.arch,
            hypervisor,
            hypervisor_version: hypervisor_version.as_deref(),
            unresponsive,
            plugin,
            plugin_installed,
            plugin_version,
            plugins_unknown: plugins_unknown.as_deref(),
            needs_admin,
            platform_problem,
        };
        let reason = unavailable_reason(provider, detection);
        statuses.push(ProviderStatus { provider, reason });
    }
    statuses
}

/// Whether `provider` is an installed Hyper-V (`hypervisor`) that this account may not manage.
/// Vagrant's Hyper-V provider refuses every command (status reads included) from such an
/// account: said here with the way out, not as a raw error mid-boot. When the runner's check
/// can't say, no reason is added.
async fn needs_admin(provider: Provider, hypervisor: Option<bool>, runner: &dyn ProcessRunner) -> bool {
    provider == Provider::Hyperv && hypervisor == Some(true) && isothrum_runner::may_manage_hyperv(runner).await == isothrum_runner::HyperVRights::Missing
}

/// Whether VirtualBox's background service is wedged: `VBoxManage --version` never talks to it, so
/// ask it something that does and see whether that times out.
///
/// A probe that times out is dropped, not awaited to its end: the process runner stops the hung
/// `VBoxManage` in the background (`SystemProcesses`: asked, then killed 30 s later), so it may
/// outlive this answer. An embedder that exits at once should await the runner's `stopped()`
/// first, as after any operation cut short.
async fn vbox_wedged(runner: &dyn ProcessRunner) -> bool {
    let probe = exec::read_any_exit(runner, "VBoxManage", &["list", "runningvms"]);
    tokio::time::timeout(VBOX_PROBE, probe).await.is_err()
}

/// What `detect` learned about one provider, grouped so `unavailable_reason` reads one value
/// rather than a handful of flags that always travel together: every probe of the machine is
/// `detect`'s, so `unavailable_reason` only decides.
#[derive(Clone, Copy)]
struct Detection<'a> {
    /// The machine's CPU (which QEMU binary is its probe).
    arch: Arch,
    /// Whether the hypervisor is installed, `None` when there is nothing to probe.
    hypervisor: Option<bool>,
    /// The hypervisor's version, when it says one.
    hypervisor_version: Option<&'a str>,
    /// Whether the hypervisor's service is wedged (its liveness probe timed out).
    unresponsive: bool,
    /// The Vagrant plugin it needs, if it is not built in.
    plugin: Option<&'a str>,
    /// Whether that plugin is installed.
    plugin_installed: bool,
    /// That plugin's version, when `vagrant plugin list` says one.
    plugin_version: Option<&'a str>,
    /// Why `vagrant plugin list` didn't answer, when it didn't.
    plugins_unknown: Option<&'a str>,
    /// Whether this account may not manage Hyper-V (Hyper-V only), as
    /// `isothrum_runner::may_manage_hyperv` answers.
    needs_admin: bool,
    /// Why this machine can't run the provider's guests although its tool is installed (libvirt:
    /// KVM's device missing or not the user's to open).
    platform_problem: Option<&'a str>,
}

/// Why a provider can't be used here, Vagrant being installed; `None` when it can: the first
/// layer that is missing.
fn unavailable_reason(provider: Provider, detection: Detection<'_>) -> Option<String> {
    let Detection {
        arch,
        hypervisor,
        hypervisor_version,
        unresponsive,
        plugin,
        plugin_installed,
        plugin_version,
        plugins_unknown,
        needs_admin,
        platform_problem,
    } = detection;
    if unresponsive {
        Some("VirtualBox isn't responding (its background service is stuck): quit VirtualBox and any VM windows, or restart the computer".to_owned())
    } else if hypervisor == Some(false) && provider == Provider::Hyperv {
        Some("Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it".to_owned())
    } else if needs_admin {
        Some(HYPERV_NEEDS_ADMIN.to_owned())
    } else if hypervisor == Some(false) {
        Some(probe(provider, arch).map_or_else(|| "not installed".to_owned(), |(program, _)| format!("`{program}` was not found")))
    } else if let Some(why) = platform_problem {
        Some(why.to_owned())
    } else if let Some((name, version, min)) = hypervisor_too_old(provider, hypervisor_version) {
        // Said before the plugin: updating the plugin wouldn't help.
        Some(format!("{name} {version} is too old: {min} or later is needed. Update {name}"))
    } else if let Some(why) = plugins_unknown.filter(|_| plugin.is_some()) {
        Some(format!("Vagrant couldn't say which plugins it has (`vagrant plugin list`: {why})"))
    } else if !plugin_installed {
        plugin.map(|p| format!("Vagrant plugin `{p}` is not installed"))
    } else if let Some((version, min)) = plugin_too_old(provider, plugin_version) {
        // Installing the plugin again updates it.
        plugin.map(|p| format!("Vagrant plugin `{p}` {version} is too old: {min} or later is needed. Install it again to update it"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
