//! The Vagrant providers (hypervisors) this computer can drive, and detecting which are usable.
//! Vagrant abstracts the hypervisor; we detect what is installed here and let `vagrant up
//! --provider` do the rest.
//!
//! The Windows "may this account manage Hyper-V" refinement needs `unsafe` Win32 (a token and SID
//! check) that this crate forbids, so [`detect`] never computes it: a caller supplies the answer
//! as `may_manage_hyperv: Option<bool>`. An embedder without such a check passes `None`,
//! which adds no admin reason; one with it passes `Some(its own unsafe check)`, and a
//! `Some(false)` folds the "needs administrator" reason into Hyper-V's status.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::ProcessRunner;

use crate::exec;

/// A Vagrant provider (the value given to `vagrant up --provider`): Isoloom's, the one its run
/// manifests name.
pub use isoloom_manifest::Provider;

/// The id Vagrant knows `provider` by (`vmware_desktop`), as the manifest writes it.
#[must_use]
pub fn id(provider: Provider) -> String {
    match serde_json::to_value(provider) {
        Ok(serde_json::Value::String(id)) => id,
        _ => String::new(),
    }
}

/// Every provider.
const ALL: [Provider; 8] = [
    Provider::Virtualbox,
    Provider::VmwareDesktop,
    Provider::Hyperv,
    Provider::Parallels,
    Provider::Libvirt,
    Provider::Qemu,
    Provider::Utm,
    Provider::VmwareEsxi,
];

/// Whether this provider can run on the current OS at all: local hypervisors are
/// platform-specific, remote ones apply anywhere.
fn applicable(provider: Provider) -> bool {
    use std::env::consts::{ARCH, OS};
    match provider {
        Provider::Virtualbox | Provider::VmwareDesktop if OS == "windows" && ARCH == "aarch64" => false,
        Provider::Hyperv => OS == "windows",
        Provider::Parallels | Provider::Utm => OS == "macos",
        Provider::Libvirt => OS == "linux",
        _ => true,
    }
}

/// The Vagrant plugin the provider needs, if it is not built in.
fn plugin(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Virtualbox | Provider::Hyperv => None,
        Provider::VmwareDesktop => Some("vagrant-vmware-desktop"),
        Provider::Parallels => Some("vagrant-parallels"),
        Provider::Libvirt => Some("vagrant-libvirt"),
        Provider::Qemu => Some("vagrant-qemu"),
        Provider::Utm => Some("vagrant_utm"),
        Provider::VmwareEsxi => Some("vagrant-vmware-esxi"),
    }
}

/// The oldest version of the provider's plugin that works: `vagrant_utm` before 0.2.0 reads
/// a VM's forwarded ports through a script UTM refuses (`-600`, `-1743`) and has no `started`
/// state.
fn plugin_minimum(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Utm => Some("0.2.0"),
        _ => None,
    }
}

/// The hypervisor's name and the oldest version its plugin drives: `vagrant_utm` refuses a
/// UTM before 4.6.5 (no import of a box, no registry for its shared folders).
fn hypervisor_minimum(provider: Provider) -> Option<(&'static str, &'static str)> {
    match provider {
        Provider::Utm => Some(("UTM", "4.6.5")),
        _ => None,
    }
}

/// The hypervisor's own version, when it is installed and says it: UTM's app bundle
/// (`CFBundleShortVersionString`); the others aren't held to a version yet.
async fn hypervisor_version(provider: Provider, runner: &dyn ProcessRunner) -> Option<String> {
    if provider != Provider::Utm {
        return None;
    }
    let plist = format!("{UTM_APP}/Contents/Info.plist");
    let out = exec::read(runner, "defaults", &["read", &plist, "CFBundleShortVersionString"]).await.ok()?;
    let version = out.trim().to_owned();
    (!version.is_empty()).then_some(version)
}

/// The local tool that proves the hypervisor itself is installed, if there is one.
fn probe(provider: Provider) -> Option<(&'static str, &'static [&'static str])> {
    match provider {
        Provider::Virtualbox => Some(("VBoxManage", &["--version"])),
        Provider::VmwareDesktop => Some(("vmrun", &[])),
        Provider::Parallels => Some(("prlctl", &["--version"])),
        Provider::Libvirt => Some(("virsh", &["--version"])),
        Provider::Qemu => Some((qemu_binary(), &["--version"])),
        _ => None,
    }
}

/// Whether the hypervisor itself is installed here, `None` when there is nothing local to
/// check (remote providers). UTM is an app bundle; Hyper-V is a Windows feature.
async fn hypervisor_installed(provider: Provider, runner: &dyn ProcessRunner) -> Option<bool> {
    match provider {
        Provider::Utm => Some(Path::new(UTM_APP).exists()),
        Provider::Hyperv => Some(hyperv_enabled()),
        _ => {
            let (program, args) = probe(provider)?;
            Some(tool_present(runner, program, args).await)
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

/// Why libvirt can't run VMs here although `virsh` is installed: vagrant-libvirt boots KVM guests,
/// so it needs `/dev/kvm`, and the user must be allowed to open it. `None` when it is fine.
fn kvm_problem() -> Option<String> {
    let dev = Path::new("/dev/kvm");
    if !dev.exists() {
        return Some("KVM isn't available on this machine (no /dev/kvm): turn on virtualization in the firmware, or use another hypervisor".to_owned());
    }
    if std::fs::OpenOptions::new().read(true).write(true).open(dev).is_err() {
        return Some("you can't use /dev/kvm yet: add yourself to the kvm and libvirt groups, then sign out and back in".to_owned());
    }
    None
}

/// The QEMU binary for this machine's CPU.
fn qemu_binary() -> &'static str {
    if std::env::consts::ARCH == "aarch64" {
        "qemu-system-aarch64"
    } else {
        "qemu-system-x86_64"
    }
}

/// One provider's readiness on this machine.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProviderStatus {
    /// The provider.
    pub provider: Provider,
    /// Whether it is ready to use here.
    pub available: bool,
    /// Why it is not available, `None` when it is.
    pub reason: Option<String>,
}

/// Each plugin of `vagrant plugin list` with its version, from lines like `vagrant-libvirt
/// (0.12.2, global)`; `None` for a line that names no version (a wrapped or cut line).
fn parse_plugin_versions(out: &str) -> Vec<(String, Option<String>)> {
    out.lines()
        .filter_map(|l| {
            let mut words = l.split_whitespace();
            let name = words.next().filter(|name| name.starts_with("vagrant-") || name.starts_with("vagrant_"))?;
            let version = words
                .next()
                .map(|w| w.trim_matches(|c: char| matches!(c, '(' | ')' | ',')).to_owned())
                .filter(|v| v.starts_with(|c: char| c.is_ascii_digit()));
            Some((name.to_owned(), version))
        })
        .collect()
}

/// Whether version `found` is `min` or later: dot-separated numbers compared in turn, a missing
/// one as 0 (`4.7` reads as `4.7.0`) and a part that doesn't start with a digit (`beta`) as 0.
fn version_at_least(found: &str, min: &str) -> bool {
    let numbers = |v: &str| -> Vec<u64> {
        v.trim()
            .split('.')
            .map(|part| part.chars().take_while(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0))
            .collect()
    };
    let (found, min) = (numbers(found), numbers(min));
    for i in 0..found.len().max(min.len()) {
        let (a, b) = (found.get(i).copied().unwrap_or(0), min.get(i).copied().unwrap_or(0));
        if a != b {
            return a > b;
        }
    }
    true
}

/// The version `installed` of `provider`'s plugin when it is older than the one that works
/// (`plugin_minimum`), with that minimum; `None` when it is recent enough, unknown, or the
/// plugin has no minimum.
fn plugin_too_old(provider: Provider, installed: Option<&str>) -> Option<(&str, &'static str)> {
    let (version, min) = installed.zip(plugin_minimum(provider))?;
    (!version_at_least(version, min)).then_some((version, min))
}

/// The version `installed` of `provider`'s hypervisor when it is older than the one its plugin
/// drives (`hypervisor_minimum`), with the hypervisor's name and that minimum.
fn hypervisor_too_old(provider: Provider, installed: Option<&str>) -> Option<(&'static str, &str, &'static str)> {
    let (version, (name, min)) = installed.zip(hypervisor_minimum(provider))?;
    (!version_at_least(version, min)).then_some((name, version, min))
}

/// Whether `program` is installed: it is, as long as it could be started (a tool that prints usage
/// and exits non-zero, like `vmrun`, still exists).
async fn tool_present(runner: &dyn ProcessRunner, program: &str, args: &[&str]) -> bool {
    exec::read(runner, program, args).await.is_ok()
}

/// The Vagrant plugins installed, each with its version when it says one; why not when `vagrant
/// plugin list` doesn't answer.
async fn installed_plugins(runner: &dyn ProcessRunner) -> Result<Vec<(String, Option<String>)>, String> {
    exec::read_ok(runner, "vagrant", &["plugin", "list"])
        .await
        .map(|out| parse_plugin_versions(&out))
}

/// How long the VirtualBox liveness probe may take before VirtualBox counts as wedged.
const VBOX_PROBE: Duration = Duration::from_secs(8);

/// Why Hyper-V can't be used by this account yet, and the two ways out. Folded into Hyper-V's
/// status when the caller's `may_manage_hyperv` is `Some(false)`.
const HYPERV_NEEDS_ADMIN: &str = "Vagrant drives Hyper-V only for an administrator or a member of the Hyper-V Administrators group: \
add this account to that group and sign out of Windows and back in, or run this program as administrator";

/// The readiness of every provider usable on this machine: its hypervisor, its plugin, and why it
/// is not available. `vagrant_installed` is global, so it is a plain argument.
///
/// `may_manage_hyperv` is the one fact this crate cannot work out without `unsafe` (see the module
/// docs): `Some(false)` folds the "needs administrator" reason into Hyper-V, `Some(true)` and
/// `None` add nothing. An embedder without a token/SID check passes `None`.
pub async fn detect(runner: &dyn ProcessRunner, vagrant_installed: bool, may_manage_hyperv: Option<bool>) -> Vec<ProviderStatus> {
    let plugins = if vagrant_installed { installed_plugins(runner).await } else { Ok(Vec::new()) };
    // Vagrant that can't list its plugins says nothing of them: one reason for every provider
    // that needs one, rather than each plugin "not installed".
    let (plugins, plugins_unknown) = match plugins {
        Ok(plugins) => (plugins, None),
        Err(why) => (Vec::new(), Some(why)),
    };
    let mut statuses = Vec::new();
    for provider in ALL {
        if !applicable(provider) {
            continue;
        }
        let hypervisor = hypervisor_installed(provider, runner).await;
        let hypervisor_version = if hypervisor == Some(true) {
            hypervisor_version(provider, runner).await
        } else {
            None
        };
        let plugin = plugin(provider);
        let installed = plugin.and_then(|needed| plugins.iter().find(|(i, _)| i == needed));
        let plugin_installed = plugin.is_none() || (vagrant_installed && installed.is_some());
        let plugin_version = installed.and_then(|(_, v)| v.as_deref());
        let unresponsive = provider == Provider::Virtualbox && hypervisor == Some(true) && vbox_wedged(runner).await;
        // Vagrant's Hyper-V provider refuses every command (status reads included) from an account
        // that may not manage Hyper-V: said here with the way out, not as a raw error mid-boot.
        let needs_admin = provider == Provider::Hyperv && hypervisor == Some(true) && may_manage_hyperv == Some(false);
        let detection = Detection {
            hypervisor,
            hypervisor_version: hypervisor_version.as_deref(),
            unresponsive,
            plugin,
            plugin_installed,
            plugin_version,
            plugins_unknown: plugins_unknown.as_deref(),
            needs_admin,
        };
        let reason = unavailable_reason(provider, vagrant_installed, detection);
        statuses.push(ProviderStatus {
            provider,
            available: reason.is_none(),
            reason,
        });
    }
    statuses
}

/// Whether VirtualBox's background service is wedged: `VBoxManage --version` never talks to it, so
/// ask it something that does and see whether that times out.
async fn vbox_wedged(runner: &dyn ProcessRunner) -> bool {
    let probe = exec::read(runner, "VBoxManage", &["list", "runningvms"]);
    tokio::time::timeout(VBOX_PROBE, probe).await.is_err()
}

/// What `detect` learned about one provider, grouped so `unavailable_reason` reads one value
/// rather than a handful of flags that always travel together.
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
    /// Whether this account may not manage Hyper-V (Hyper-V only; the caller's injected answer).
    needs_admin: bool,
}

/// Why a provider can't be used here, `None` when it can: the first layer that is missing.
fn unavailable_reason(provider: Provider, vagrant_installed: bool, detection: Detection<'_>) -> Option<String> {
    let Detection {
        hypervisor,
        hypervisor_version,
        unresponsive,
        plugin,
        plugin_installed,
        plugin_version,
        plugins_unknown,
        needs_admin,
    } = detection;
    if !vagrant_installed {
        Some("Vagrant is not installed".to_owned())
    } else if unresponsive {
        Some(
            "VirtualBox isn't responding (its background service is stuck). Quit VirtualBox and any VM windows, or restart the computer, then re-check."
                .to_owned(),
        )
    } else if hypervisor == Some(false) && provider == Provider::Hyperv {
        Some("Hyper-V isn't turned on (Windows Features > Hyper-V), or this edition of Windows doesn't have it".to_owned())
    } else if needs_admin {
        Some(HYPERV_NEEDS_ADMIN.to_owned())
    } else if hypervisor == Some(false) {
        Some(probe(provider).map_or_else(|| "not installed".to_owned(), |(program, _)| format!("`{program}` was not found")))
    } else if let Some(why) = (provider == Provider::Libvirt).then(kvm_problem).flatten() {
        Some(why)
    } else if let Some((name, version, min)) = hypervisor_too_old(provider, hypervisor_version) {
        // Said before the plugin: updating the plugin wouldn't help.
        Some(format!("{name} {version} is too old: {min} or later is needed. Update {name}, then re-check"))
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
    use isothrum_runner::process::fake::{FakeProcesses, Reply};

    use super::*;

    #[test]
    fn a_providers_id_is_the_one_vagrant_and_the_manifest_use() {
        let ids: Vec<String> = ALL.into_iter().map(id).collect();
        assert_eq!(
            ids,
            ["virtualbox", "vmware_desktop", "hyperv", "parallels", "libvirt", "qemu", "utm", "vmware_esxi"]
        );
    }

    #[test]
    fn plugin_lines_carry_their_versions() {
        let out = "vagrant-libvirt (0.12.2, global)\nvagrant_utm (0.2.0, global)\n  - Version Constraint: > 0\nvagrant-vmware-esxi\nvagrant-qemu (0.7.0)\n";
        assert_eq!(
            parse_plugin_versions(out),
            [
                ("vagrant-libvirt".to_owned(), Some("0.12.2".to_owned())),
                ("vagrant_utm".to_owned(), Some("0.2.0".to_owned())),
                ("vagrant-vmware-esxi".to_owned(), None),
                ("vagrant-qemu".to_owned(), Some("0.7.0".to_owned())),
            ]
        );
    }

    #[test]
    fn versions_compare_by_their_numbers() {
        assert!(version_at_least("4.7.5", "4.6.5"));
        assert!(version_at_least("4.6.5", "4.6.5"));
        assert!(!version_at_least("4.5.1", "4.6.5"));
        assert!(version_at_least("4.7", "4.6.5"), "a missing part is 0");
        assert!(!version_at_least("0.1.6", "0.2.0"));
        assert!(version_at_least("0.2.0-beta", "0.2.0"), "a suffix counts as 0");
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
        };
        assert_eq!(plugin_too_old(Provider::Utm, Some("0.1.6")), Some(("0.1.6", "0.2.0")));
        assert_eq!(
            unavailable_reason(Provider::Utm, true, detection(Some("4.7.5"), Some("0.1.6"), true)).as_deref(),
            Some("Vagrant plugin `vagrant_utm` 0.1.6 is too old: 0.2.0 or later is needed. Install it again to update it")
        );
        // A UTM the plugin refuses, said before the plugin (updating the plugin wouldn't help).
        assert_eq!(
            unavailable_reason(Provider::Utm, true, detection(Some("4.5.1"), Some("0.1.6"), true)).as_deref(),
            Some("UTM 4.5.1 is too old: 4.6.5 or later is needed. Update UTM, then re-check")
        );
        // Recent enough, or no version to judge: nothing stands in the way.
        assert_eq!(unavailable_reason(Provider::Utm, true, detection(Some("4.7.5"), Some("0.2.0"), true)), None);
        assert_eq!(unavailable_reason(Provider::Utm, true, detection(None, None, true)), None);
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
            ("vagrant plugin list", Reply::ok("vagrant-qemu (0.6.3, global)")),
        ]);
        let qemu = detect(&with, true, None).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert!(qemu.available && qemu.reason.is_none(), "{qemu:?}");

        let without = FakeProcesses::rules(&[("qemu-system", Reply::ok("QEMU 10.0")), ("vagrant plugin list", Reply::ok(""))]);
        let no_plugin = detect(&without, true, None).await.into_iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert_eq!(
            (no_plugin.available, no_plugin.reason.as_deref()),
            (false, Some("Vagrant plugin `vagrant-qemu` is not installed")),
        );
    }

    #[tokio::test]
    async fn a_vagrant_that_cant_list_its_plugins_is_one_reason_not_each_plugin_missing() {
        let broken = FakeProcesses::rules(&[
            ("qemu-system", Reply::ok("QEMU 10.0")),
            ("VBoxManage", Reply::ok("7.1.4")),
            ("vagrant plugin list", Reply::fail(1, "Vagrant failed to initialize at a very early stage")),
        ]);
        let statuses = detect(&broken, true, None).await;
        let qemu = statuses.iter().find(|s| s.provider == Provider::Qemu).unwrap();
        assert_eq!(
            qemu.reason.as_deref(),
            Some("Vagrant couldn't say which plugins it has (`vagrant plugin list`: Vagrant failed to initialize at a very early stage)")
        );
        // A provider built into Vagrant needs no plugin: what Vagrant lists doesn't matter to it.
        let virtualbox = statuses.iter().find(|s| s.provider == Provider::Virtualbox);
        assert!(virtualbox.is_none_or(|s| s.available), "{virtualbox:?}");
    }

    #[test]
    fn hyperv_needs_admin_only_when_the_caller_says_the_account_cannot_manage_it() {
        // Hyper-V's `applicable()` gate keeps it out of `detect` off Windows, so the injected
        // `needs_admin` is tested through `unavailable_reason` directly.
        let hyperv = |needs_admin| Detection {
            hypervisor: Some(true),
            hypervisor_version: None,
            unresponsive: false,
            plugin: None,
            plugin_installed: true,
            plugin_version: None,
            plugins_unknown: None,
            needs_admin,
        };
        assert_eq!(unavailable_reason(Provider::Hyperv, true, hyperv(true)).as_deref(), Some(HYPERV_NEEDS_ADMIN));
        assert_eq!(unavailable_reason(Provider::Hyperv, true, hyperv(false)), None);
    }
}
