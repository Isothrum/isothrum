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

use isoloom_manifest::{Arch, HostOs};
use isothrum_runner::{ProcessRunner, this_arch, this_host, version_found};

use crate::exec;

/// A Vagrant provider (the value given to `vagrant up --provider`): Isoloom's, the one its run
/// manifests name.
pub use isoloom_manifest::Provider;

/// Every provider Isoloom names, in its order: read from its type, so one Isoloom adds is
/// detected without a list here to keep in step.
fn all() -> Vec<Provider> {
    isothrum_runner::every_name::<Provider>()
        .iter()
        .filter_map(|name| isothrum_runner::from_wire_name(name))
        .collect()
}

/// Whether this provider can run on the current OS and CPU at all ([`applicable_on`]).
fn applicable(provider: Provider) -> bool {
    applicable_on(provider, this_host(), this_arch())
}

/// How this computer tells a provider's hypervisor is installed.
#[derive(Debug, Clone, Copy)]
enum Check {
    /// A tool that proves it: installed as long as it starts.
    Tool(&'static str, &'static [&'static str]),
    /// QEMU's system emulator for this machine's CPU ([`qemu_binary`]).
    Qemu,
    /// UTM's app bundle, which also says its version.
    UtmApp,
    /// The Hyper-V Windows feature.
    HypervFeature,
    /// Nothing local to check: a remote hypervisor.
    Remote,
}

/// What detection knows of one provider, one row each ([`facts`]): a provider Isoloom adds is
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

/// What detection knows of `provider`: a provider Isoloom adds fails to compile here until it
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

/// The local tool that proves the hypervisor itself is installed, if there is one.
fn probe(provider: Provider) -> Option<(&'static str, &'static [&'static str])> {
    match facts(provider).check {
        Check::Tool(program, args) => Some((program, args)),
        Check::Qemu => Some((qemu_binary(), &["--version"])),
        Check::UtmApp | Check::HypervFeature | Check::Remote => None,
    }
}

/// Whether the hypervisor itself is installed here, `None` when there is nothing local to
/// check (remote providers). UTM is an app bundle; Hyper-V is a Windows feature.
async fn hypervisor_installed(provider: Provider, runner: &dyn ProcessRunner) -> Option<bool> {
    match facts(provider).check {
        Check::UtmApp => Some(Path::new(UTM_APP).exists()),
        Check::HypervFeature => Some(hyperv_enabled()),
        Check::Tool(..) | Check::Qemu | Check::Remote => {
            let (program, args) = probe(provider)?;
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

/// The QEMU binary for this machine's CPU.
fn qemu_binary() -> &'static str {
    qemu_binary_on(this_arch())
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

/// The readiness of every provider that applies to this computer's OS and CPU, usable or not:
/// `None` when it is ready, else why not (its hypervisor, its plugin, Hyper-V needing an
/// administrator...). Without Vagrant (`vagrant_installed`, the caller's to know) each says so,
/// and nothing is probed.
pub async fn detect(runner: &dyn ProcessRunner, vagrant_installed: bool) -> Vec<ProviderStatus> {
    if !vagrant_installed {
        return all()
            .into_iter()
            .filter(|p| applicable(*p))
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
        if !applicable(provider) {
            continue;
        }
        let hypervisor = hypervisor_installed(provider, runner).await;
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
        let platform_problem = match provider {
            Provider::Libvirt => kvm_problem(Path::new(KVM)),
            Provider::Virtualbox if hypervisor == Some(true) => vbox_driver_problem(runner).await,
            _ => None,
        };
        let platform_problem = platform_problem.as_deref();
        let detection = Detection {
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
    provider == Provider::Hyperv && hypervisor == Some(true) && isothrum_runner::may_manage_hyperv(runner).await == Some(false)
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
        Some(probe(provider).map_or_else(|| "not installed".to_owned(), |(program, _)| format!("`{program}` was not found")))
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
mod tests {
    use isothrum_runner::fake::{FakeProcesses, Reply};

    use super::*;

    /// The names Isoloom's `Provider` takes, sorted.
    fn isoloom_providers() -> Vec<String> {
        let mut names = isothrum_runner::every_name::<Provider>();
        names.sort();
        names
    }

    /// Detection covers every provider Isoloom names: one added there fails this until it is
    /// listed.
    #[test]
    fn every_provider_isoloom_names_is_listed() {
        let mut listed: Vec<String> = all().iter().map(isothrum_runner::wire_name).collect();
        listed.sort();
        assert_eq!(listed, isoloom_providers());
    }

    /// The problem of a device the user may not open: one only its owner reads (root reads any,
    /// so as root the text is the one it would be).
    fn denied_device(dir: &Path) -> String {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let device = dir.join("denied");
            std::fs::write(&device, "").unwrap();
            std::fs::set_permissions(&device, std::fs::Permissions::from_mode(0o400)).unwrap();
            if let Some(said) = kvm_problem(&device) {
                assert!(said.starts_with("you can't use /dev/kvm yet"), "{said}");
                return said;
            }
        }
        let _ = dir;
        "you can't use /dev/kvm yet: add yourself to the kvm and libvirt groups, then sign out and back in".to_owned()
    }

    /// Each provider on each OS and CPU, not only this host's.
    #[test]
    fn a_provider_applies_where_its_hypervisor_runs() {
        let platforms = [
            (HostOs::Linux, Arch::Amd64),
            (HostOs::Linux, Arch::Arm64),
            (HostOs::Macos, Arch::Arm64),
            (HostOs::Macos, Arch::Amd64),
            (HostOs::Windows, Arch::Amd64),
            (HostOs::Windows, Arch::Arm64),
        ];
        let table = [
            (Provider::Virtualbox, [true, true, true, true, true, false]),
            (Provider::VmwareDesktop, [true, true, true, true, true, false]),
            (Provider::Hyperv, [false, false, false, false, true, true]),
            (Provider::Parallels, [false, false, true, true, false, false]),
            (Provider::Utm, [false, false, true, true, false, false]),
            (Provider::Libvirt, [true, true, false, false, false, false]),
            (Provider::Qemu, [true; 6]),
            (Provider::VmwareEsxi, [true; 6]),
        ];
        assert_eq!(table.len(), all().len(), "every provider is in the table");
        for (provider, expected) in table {
            for ((os, arch), applies) in platforms.iter().zip(expected) {
                assert_eq!(applicable_on(provider, *os, *arch), applies, "{provider:?} on {os:?} {arch:?}");
            }
        }
        assert_eq!(qemu_binary_on(Arch::Arm64), "qemu-system-aarch64");
        assert_eq!(qemu_binary_on(Arch::Amd64), "qemu-system-x86_64");
    }

    #[test]
    fn a_providers_id_is_the_one_vagrant_and_the_manifest_use() {
        let ids: Vec<String> = all().iter().map(isothrum_runner::wire_name).collect();
        assert_eq!(
            ids,
            ["virtualbox", "vmware_desktop", "hyperv", "parallels", "libvirt", "qemu", "utm", "vmware_esxi"]
        );
    }

    #[test]
    fn utm_and_its_plugin_are_held_to_a_version() {
        let detection = |hv: Option<&'static str>, pl: Option<&'static str>, installed: bool| Detection {
            hypervisor: Some(true),
            hypervisor_version: hv,
            unresponsive: false,
            plugin: Some("vagrant_utm"),
            plugin_installed: installed,
            plugin_version: pl,
            plugins_unknown: None,
            needs_admin: false,
            platform_problem: None,
        };
        assert_eq!(plugin_too_old(Provider::Utm, Some("0.1.6")), Some(("0.1.6", "0.2.0")));
        // Regression: a suffix was dropped, so a pre-release of the minimum passed for it; it comes
        // before its release, as the runner's tool probes (Isoloom's Version) order it.
        assert_eq!(plugin_too_old(Provider::Utm, Some("0.2.0-beta")), Some(("0.2.0-beta", "0.2.0")));
        assert_eq!(plugin_too_old(Provider::Utm, Some("unknown")), None, "no version to judge");
        assert_eq!(
            unavailable_reason(Provider::Utm, detection(Some("4.7.5"), Some("0.1.6"), true)).as_deref(),
            Some("Vagrant plugin `vagrant_utm` 0.1.6 is too old: 0.2.0 or later is needed. Install it again to update it")
        );
        // A UTM the plugin refuses, said before the plugin (updating the plugin wouldn't help).
        assert_eq!(
            unavailable_reason(Provider::Utm, detection(Some("4.5.1"), Some("0.1.6"), true)).as_deref(),
            Some("UTM 4.5.1 is too old: 4.6.5 or later is needed. Update UTM")
        );
        // Recent enough, or no version to judge: nothing stands in the way.
        assert_eq!(unavailable_reason(Provider::Utm, detection(Some("4.7.5"), Some("0.2.0"), true)), None);
        assert_eq!(unavailable_reason(Provider::Utm, detection(None, None, true)), None);
        assert_eq!(plugin_too_old(Provider::Utm, None), None);
        // Other providers aren't held to a version.
        assert_eq!(plugin_too_old(Provider::Qemu, Some("0.1.0")), None);
        assert_eq!(hypervisor_too_old(Provider::Virtualbox, Some("6.0")), None);
    }

    #[tokio::test]
    async fn detect_reports_a_ready_qemu_and_an_unavailable_one_by_its_plugin() {
        // QEMU installed, its plugin present: available. A missing plugin makes it unavailable.
        let with = FakeProcesses::rules(&[
            ("qemu-system", Reply::ok("QEMU 10.0")),
            (
                "vagrant plugin list --machine-readable",
                Reply::ok("1,,plugin-name,vagrant-qemu\n1,vagrant-qemu,plugin-version,0.6.3%!(VAGRANT_COMMA) global"),
            ),
        ]);
        let qemu = detect(&with, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert!(qemu.reason.is_none(), "{qemu:?}");

        let without = FakeProcesses::rules(&[("qemu-system", Reply::ok("QEMU 10.0")), ("vagrant plugin list", Reply::ok(""))]);
        let no_plugin = detect(&without, true).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert_eq!(no_plugin.reason.as_deref(), Some("Vagrant plugin `vagrant-qemu` is not installed"),);
    }

    #[tokio::test]
    async fn a_vagrant_that_cant_list_its_plugins_is_one_reason_not_each_plugin_missing() {
        let broken = FakeProcesses::rules(&[
            ("qemu-system", Reply::ok("QEMU 10.0")),
            ("VBoxManage", Reply::ok("7.1.4")),
            ("vagrant plugin list", Reply::fail(1, "Vagrant failed to initialize at a very early stage")),
        ]);
        let statuses = detect(&broken, true).await;
        let qemu = statuses.iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert_eq!(
            qemu.reason.as_deref(),
            Some("Vagrant couldn't say which plugins it has (`vagrant plugin list`: Vagrant failed to initialize at a very early stage (exited with code 1))")
        );
        // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
        let virtualbox = statuses.iter().find(|s| s.provider == Provider::Virtualbox);
        assert!(virtualbox.is_none_or(|s| s.reason.is_none()), "{virtualbox:?}");
    }

    /// Each missing layer is said for what it is: Hyper-V turned off (a Windows feature, not a
    /// program), a hypervisor whose program isn't installed.
    #[test]
    fn a_hypervisor_that_is_off_or_not_installed_says_which() {
        let absent = Detection {
            hypervisor: Some(false),
            hypervisor_version: None,
            unresponsive: false,
            plugin: None,
            plugin_installed: true,
            plugin_version: None,
            plugins_unknown: None,
            needs_admin: false,
            platform_problem: None,
        };
        assert_eq!(
            unavailable_reason(Provider::Hyperv, absent).as_deref(),
            Some("Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it")
        );
        assert_eq!(unavailable_reason(Provider::Virtualbox, absent).as_deref(), Some("`VBoxManage` was not found"));
    }

    /// libvirt needs KVM's device, and the user's leave to open it: each is said, read once by
    /// `detect`, and comes after the tool itself (no `virsh` is said first).
    #[test]
    fn libvirt_without_kvm_or_its_leave_says_which() {
        let dir = tempfile::tempdir().unwrap();
        let missing = kvm_problem(&dir.path().join("kvm")).unwrap();
        assert!(missing.starts_with("KVM isn't available on this machine (no /dev/kvm)"), "{missing}");
        // Regression: any failure to open it (a folder here; EBUSY, ENXIO on a real device) was
        // said as a device the user may not open. Unix only: Windows refuses to open a folder
        // with "access denied", the permission problem's own kind.
        #[cfg(unix)]
        {
            let other = kvm_problem(dir.path()).unwrap();
            assert!(other.starts_with("/dev/kvm can't be opened: "), "{other}");
        }
        let device = dir.path().join("device");
        std::fs::write(&device, "").unwrap();
        assert_eq!(kvm_problem(&device), None);
        let denied = denied_device(dir.path());
        let (missing, denied) = (missing.as_str(), denied.as_str());
        let libvirt = |hypervisor, platform_problem| Detection {
            hypervisor: Some(hypervisor),
            hypervisor_version: None,
            unresponsive: false,
            plugin: Some("vagrant-libvirt"),
            plugin_installed: true,
            plugin_version: None,
            plugins_unknown: None,
            needs_admin: false,
            platform_problem,
        };
        assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, Some(missing))).as_deref(), Some(missing));
        assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, Some(denied))).as_deref(), Some(denied));
        assert_eq!(
            unavailable_reason(Provider::Libvirt, libvirt(false, Some(missing))).as_deref(),
            Some("`virsh` was not found")
        );
        assert_eq!(unavailable_reason(Provider::Libvirt, libvirt(true, None)), None);
    }

    #[tokio::test]
    async fn a_virtualbox_that_isnt_installed_is_detected_as_such() {
        let tools = FakeProcesses::rules(&[("VBoxManage", Reply::missing())]);
        let statuses = detect(&tools, true).await;
        let virtualbox = statuses.iter().find(|s| s.provider == Provider::Virtualbox);
        assert!(
            virtualbox.is_none_or(|s| s.reason.as_deref() == Some("`VBoxManage` was not found")),
            "{virtualbox:?}"
        );
    }

    #[tokio::test]
    async fn without_vagrant_every_provider_says_so_and_nothing_is_probed() {
        let tools = FakeProcesses::rules(&[]);
        let statuses = detect(&tools, false).await;
        assert_eq!(statuses.len(), all().iter().filter(|p| applicable(**p)).count());
        assert!(statuses.iter().all(|s| s.reason.as_deref() == Some(NO_VAGRANT)), "{statuses:?}");
        assert_eq!(tools.calls(), [], "no probe runs without Vagrant");
    }

    /// A tool that never answers.
    struct Stuck;

    impl ProcessRunner for Stuck {
        fn run<'a>(
            &'a self,
            _spec: &'a isothrum_runner::ProcessSpec,
            _on_line: isothrum_runner::OnLine<'a>,
        ) -> isothrum_runner::BoxFuture<'a, Result<isothrum_runner::Exit, isothrum_runner::ProcessError>> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(isothrum_runner::Exit::new(Some(0)))
            })
        }
    }

    /// Regression (#128): with its kernel driver not loaded, `VBoxManage` still answers, after a
    /// warning, and VirtualBox read as ready until `vagrant up` failed.
    #[tokio::test]
    async fn a_virtualbox_whose_kernel_driver_isnt_loaded_isnt_ready() {
        let oracle = "WARNING: The vboxdrv kernel module is not loaded. Either there is no module\n         available for the current kernel (6.8.0-49-generic) or it failed to\n         load. Please recompile the kernel module and install it by\n\n           sudo /sbin/vboxconfig\n\n         You will not be able to start VMs until this problem is fixed.\n7.0.14r161095";
        let distribution = "WARNING: The character device /dev/vboxdrv does not exist.\n\t Please install the virtualbox-dkms package and the appropriate\n\t headers, most likely linux-headers-6.8.0-49-generic.\n\n\t You will not be able to start VMs until this problem is fixed.\n7.0.16_Ubuntur162802";
        for said in [oracle, distribution] {
            assert!(vbox_driver_missing(said), "{said}");
            let tools = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok(said))]);
            assert_eq!(vbox_driver_problem(&tools).await.as_deref(), Some(VBOX_DRIVER_MISSING));
            // Printed on standard error, it is read too.
            let mut on_stderr = Reply::ok("7.0.14r161095");
            on_stderr.stderr = said.lines().map(str::to_owned).collect();
            let warned_on_stderr = FakeProcesses::rules(&[("VBoxManage --version", on_stderr)]);
            assert_eq!(vbox_driver_problem(&warned_on_stderr).await.as_deref(), Some(VBOX_DRIVER_MISSING));
        }
        let fine = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok("7.1.4r165100"))]);
        assert_eq!(vbox_driver_problem(&fine).await, None);
        if applicable(Provider::Virtualbox) {
            let detected = FakeProcesses::rules(&[("VBoxManage --version", Reply::ok(oracle)), ("vagrant plugin list", Reply::ok(""))]);
            let virtualbox = detect(&detected, true).await.into_iter().find(|s| s.provider == Provider::Virtualbox).unwrap();
            assert_eq!(virtualbox.reason.as_deref(), Some(VBOX_DRIVER_MISSING));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_virtualbox_whose_service_never_answers_is_wedged() {
        assert!(vbox_wedged(&Stuck).await);
        let answering = FakeProcesses::rules(&[("VBoxManage list runningvms", Reply::ok(""))]);
        assert!(!vbox_wedged(&answering).await);
    }

    #[tokio::test]
    async fn only_utm_is_asked_its_version() {
        let tools = FakeProcesses::rules(&[("defaults read", Reply::ok("4.7.5\n"))]);
        assert_eq!(hypervisor_version(Provider::Utm, &tools).await.as_deref(), Some("4.7.5"));
        assert_eq!(hypervisor_version(Provider::Qemu, &tools).await, None);
        let silent = FakeProcesses::rules(&[("defaults read", Reply::ok(""))]);
        assert_eq!(hypervisor_version(Provider::Utm, &silent).await, None, "no version said, none read");
        let call = tools.calls().into_iter().next().unwrap();
        assert_eq!(call.args, ["read", "/Applications/UTM.app/Contents/Info.plist", "CFBundleShortVersionString"]);
    }

    #[tokio::test]
    async fn an_installed_hyperv_needs_admin_only_when_whoami_says_the_account_may_not_manage_it() {
        let groups = |sid: &str| format!("\"Label\",\"Label\",\"{sid}\",");
        let not_allowed = FakeProcesses::rules(&[("whoami /groups", Reply::ok(&groups("S-1-16-8192")))]);
        let allowed = FakeProcesses::rules(&[("whoami /groups", Reply::ok(&groups("S-1-5-32-578")))]);
        let unknown = FakeProcesses::rules(&[("whoami", Reply::missing())]);
        assert!(needs_admin(Provider::Hyperv, Some(true), &not_allowed).await);
        assert!(!needs_admin(Provider::Hyperv, Some(true), &allowed).await);
        assert!(!needs_admin(Provider::Hyperv, Some(true), &unknown).await, "no answer adds no reason");
        assert!(!needs_admin(Provider::Hyperv, Some(false), &not_allowed).await, "not installed says so instead");
        assert!(!needs_admin(Provider::Virtualbox, Some(true), &not_allowed).await);
        assert_eq!(not_allowed.calls().len(), 1, "whoami is asked for an installed Hyper-V only");
    }

    #[test]
    fn hyperv_needs_admin_folds_into_its_reason() {
        // Hyper-V's `applicable()` gate keeps it out of `detect` off Windows, so `needs_admin` is
        // tested on its own (above) and through `unavailable_reason` here.
        let hyperv = |needs_admin| Detection {
            hypervisor: Some(true),
            hypervisor_version: None,
            unresponsive: false,
            plugin: None,
            plugin_installed: true,
            plugin_version: None,
            plugins_unknown: None,
            needs_admin,
            platform_problem: None,
        };
        assert_eq!(unavailable_reason(Provider::Hyperv, hyperv(true)).as_deref(), Some(HYPERV_NEEDS_ADMIN));
        assert_eq!(unavailable_reason(Provider::Hyperv, hyperv(false)), None);
    }
}
