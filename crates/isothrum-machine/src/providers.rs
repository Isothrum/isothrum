//! The Vagrant providers (hypervisors) this computer can drive, and detecting which are usable.
//! Vagrant abstracts the hypervisor; this detects what is installed here, what is still missing
//! for Vagrant to drive it ([`Need`]), and when it can't run VMs yet, why ([`Unavailable`], a code
//! a UI translates).
//!
//! Each provider is one row ([`ProviderRow::of`]): where it runs, how its hypervisor is found, the
//! versions it is held to. Every tool runs through the [`Context`]'s runner, within its probe time
//! limit; the files read instead (KVM's device, the kernel's modules, UTM's bundle, Hyper-V's
//! service) are the [`Host`]'s [`Devices`](crate::Devices). Whether this account may manage
//! Hyper-V is the runner's own check ([`isothrum_runner::may_manage_hyperv`], `whoami /groups`),
//! the one it decides an elevated session by.

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::exec;
use crate::host::{Arch, Host, Os};

/// A Vagrant provider (the value given to `vagrant up --provider`).
pub use isoloom_manifest::Provider;

mod hyperv;
mod kvm;
mod needs;
mod unavailable;
mod vbox;

pub use kvm::{kvm_group, login_groups, uses_kvm};
pub use needs::{Need, UtilityState, needs, vmware_utility};
pub use unavailable::Unavailable;

/// Every provider, in its type's order: read from the type, so one added there is detected
/// without a list here to keep in step.
fn all() -> Vec<Provider> {
    isothrum_runner::every_name::<Provider>()
        .iter()
        .filter_map(|name| isothrum_runner::from_wire_name(name))
        .collect()
}

/// How this computer tells a provider's hypervisor is installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    /// A tool that proves it: installed as long as it starts.
    Tool(&'static str, &'static [&'static str]),
    /// QEMU's system emulator for this machine's CPU ([`Arch::qemu_system`]).
    Qemu,
    /// UTM's app bundle ([`crate::Devices::utm_app`]), which also says its version.
    UtmApp,
    /// The Hyper-V Windows feature ([`crate::Devices::hyperv_service`]).
    HypervFeature,
    /// Nothing local to check: a remote hypervisor.
    Remote,
}

/// A provider's row of the hypervisor table ([`ProviderRow::of`]): where it runs, where its VMs
/// share a folder of this computer, where its vendor offers it, the versions it is held to, and
/// how its hypervisor is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProviderRow {
    /// The OSes it exists on.
    pub oses: &'static [Os],
    /// Whether it is built for Windows on Arm.
    pub windows_on_arm: bool,
    /// The Mac CPU it is made for, when it is made for one: offered on the other only once
    /// installed ([`offered`]).
    pub mac_cpu: Option<Arch>,
    /// Where its vendor offers it, when a setup doesn't install it (behind a login, a licence, or
    /// a Windows feature the user turns on by hand).
    pub vendor: Option<&'static str>,
    /// The OSes where its VMs share a folder of this computer two-way.
    pub shares_folders: &'static [Os],
    /// The oldest version of its plugin that works: `vagrant_utm` before 0.2.0 reads a VM's
    /// forwarded ports through a script UTM refuses (`-600`, `-1743`) and has no `started` state.
    pub plugin_minimum: Option<&'static str>,
    /// Its hypervisor's name and the oldest version its plugin drives: `vagrant_utm` refuses a UTM
    /// before 4.6.5 (no import of a box, no registry for its shared folders).
    pub hypervisor_minimum: Option<(&'static str, &'static str)>,
    /// How its hypervisor is found.
    check: Check,
}

impl ProviderRow {
    /// `provider`'s row: a provider added to the type is a compile error here until its row says
    /// where it runs.
    #[must_use]
    pub fn of(provider: Provider) -> ProviderRow {
        // A hypervisor built for every OS and CPU, its VMs sharing a folder two-way, held to no
        // version; each row says how it differs.
        const EVERYWHERE: ProviderRow = ProviderRow {
            oses: &Os::ALL,
            windows_on_arm: true,
            mac_cpu: None,
            vendor: None,
            shares_folders: &Os::ALL,
            plugin_minimum: None,
            hypervisor_minimum: None,
            check: Check::Remote,
        };
        const fn found_by(check: Check) -> ProviderRow {
            ProviderRow { check, ..EVERYWHERE }
        }
        match provider {
            // Neither ships for Windows on Arm (no installer, no Arm VMs there). VirtualBox runs a
            // Mac's VMs on Intel only.
            Provider::Virtualbox => ProviderRow {
                windows_on_arm: false,
                mac_cpu: Some(Arch::X86_64),
                ..found_by(Check::Tool("VBoxManage", &["--version"]))
            },
            Provider::VmwareDesktop => ProviderRow {
                windows_on_arm: false,
                vendor: Some("https://www.vmware.com/products/desktop-hypervisor/workstation-and-fusion"),
                ..found_by(Check::Tool("vmrun", &[]))
            },
            // Hyper-V's SMB folders ask for a password at every start.
            Provider::Hyperv => ProviderRow {
                oses: &[Os::Windows],
                vendor: Some("https://learn.microsoft.com/virtualization/hyper-v-on-windows/quick-start/enable-hyper-v"),
                shares_folders: &[],
                ..found_by(Check::HypervFeature)
            },
            Provider::Parallels => ProviderRow {
                oses: &[Os::Macos],
                vendor: Some("https://www.parallels.com/products/desktop/"),
                ..found_by(Check::Tool("prlctl", &["--version"]))
            },
            // UTM is the hypervisor made for Apple silicon.
            Provider::Utm => ProviderRow {
                oses: &[Os::Macos],
                mac_cpu: Some(Arch::Aarch64),
                plugin_minimum: Some("0.2.0"),
                hypervisor_minimum: Some(("UTM", "4.6.5")),
                ..found_by(Check::UtmApp)
            },
            Provider::Libvirt => ProviderRow {
                oses: &[Os::Linux],
                ..found_by(Check::Tool("virsh", &["--version"]))
            },
            // vagrant-qemu builds a networked VM's cloud-init seed with `oscdimg` on Windows (the
            // Windows ADK's), and Isoloom gives the qemu provider Linux and macOS hosts only; it
            // shares a folder two-way through virtiofsd, on Linux only.
            Provider::Qemu => ProviderRow {
                oses: &[Os::Macos, Os::Linux],
                shares_folders: &[Os::Linux],
                ..found_by(Check::Qemu)
            },
            // A server: reached from any computer, sharing none of its folders.
            Provider::VmwareEsxi => ProviderRow {
                shares_folders: &[],
                ..EVERYWHERE
            },
        }
    }

    /// Whether it runs VMs on another computer: nothing of it is installed here.
    #[must_use]
    pub fn remote(&self) -> bool {
        self.check == Check::Remote
    }
}

/// Whether `provider` can run on `host` at all: on its OS, and on Windows on Arm, built for it.
/// Remote ones apply anywhere.
#[must_use]
pub fn applicable(provider: Provider, host: &Host) -> bool {
    let row = ProviderRow::of(provider);
    row.oses.contains(&host.os) && (row.windows_on_arm || !host.is_windows_on_arm())
}

/// Whether `provider` is offered on `host` for VMs: applicable there, and, on a Mac, the
/// hypervisor made for its CPU (UTM on Apple silicon, VirtualBox on an Intel Mac). One already
/// `installed` is offered wherever it applies: the user chose it.
#[must_use]
pub fn offered(provider: Provider, host: &Host, installed: bool) -> bool {
    let for_this_mac = host.os != Os::Macos || ProviderRow::of(provider).mac_cpu.is_none_or(|cpu| cpu == host.arch);
    applicable(provider, host) && (installed || for_this_mac)
}

/// Whether `provider`'s VMs need `mkisofs` on `os`: QEMU's on Linux. vagrant-qemu gives a VM on a
/// private network its settings in a cloud-init seed, an ISO Vagrant builds with `mkisofs` on Linux
/// (`hdiutil` on macOS, which every Mac has). Without it, `vagrant up` fails at the first VM.
#[must_use]
pub fn needs_mkisofs(provider: Provider, os: Os) -> bool {
    provider == Provider::Qemu && os == Os::Linux
}

/// Whether Vagrant drives `provider` through HashiCorp's Vagrant VMware Utility, a system service
/// of its own (installed apart from the plugin, by an administrator) that the
/// `vagrant-vmware-desktop` plugin asks to run VMware: without it every `vagrant up` fails.
#[must_use]
pub fn needs_vmware_utility(provider: Provider) -> bool {
    provider == Provider::VmwareDesktop
}

/// The local tool that proves `provider`'s hypervisor is installed on a CPU `arch`, if one does.
fn probe_of(provider: Provider, arch: Arch) -> Option<(&'static str, &'static [&'static str])> {
    match ProviderRow::of(provider).check {
        Check::Tool(program, args) => Some((program, args)),
        Check::Qemu => Some((arch.qemu_system(), &["--version"])),
        Check::UtmApp | Check::HypervFeature | Check::Remote => None,
    }
}

/// What the account lacks to use a provider installed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub enum AccountLacks {
    /// The right to manage Hyper-V (Hyper-V only).
    HypervAdmin,
    /// A new login: it joined libvirt's groups, or `/dev/kvm`'s (QEMU on Linux), since logging
    /// in.
    NewLogin,
}

/// One provider's readiness on this machine: whether VMs can run on it here, and what is still
/// missing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ProviderStatus {
    /// The provider.
    pub provider: Provider,
    /// A server, run elsewhere: nothing of it is installed here.
    pub remote: bool,
    /// Whether VMs can run on it now (`reason` says why not).
    pub available: bool,
    /// Whether the hypervisor itself is installed (for Hyper-V, whether the Windows feature is
    /// on; for QEMU on Linux, with the `mkisofs` its VMs need, [`needs_mkisofs`]). `None` where
    /// nothing local is checked: a server.
    pub hypervisor: Option<bool>,
    /// The Vagrant plugin it needs, if it is not built in.
    pub plugin: Option<String>,
    /// Whether that plugin is installed at a version that works (true when the provider is built
    /// in): an older one reads as not installed, so a setup installs it again, which updates it.
    pub plugin_installed: bool,
    /// Where HashiCorp's Vagrant VMware Utility stands; `None` when the provider doesn't use it.
    pub utility: Option<UtilityState>,
    /// What is still to install for Vagrant to drive it here ([`needs`]): empty when nothing is.
    pub needs: Vec<Need>,
    /// Why it can't run VMs yet, `None` when it can.
    pub reason: Option<Unavailable>,
    /// What this account lacks to use it although it is installed (the right to manage Hyper-V,
    /// a new login after joining libvirt's groups), whatever else is missing.
    pub account_lacks: Option<AccountLacks>,
}

/// The readiness of every provider offered on the context's host ([`offered`]), usable or not, in
/// the provider type's order. Without Vagrant (`vagrant_installed`, the caller's to know) the
/// hypervisors are still probed, and each status says Vagrant is needed.
pub async fn detect(cx: &Context, vagrant_installed: bool) -> Vec<ProviderStatus> {
    let mut asked = Asked::of(cx, vagrant_installed).await;
    let mut statuses = Vec::new();
    for provider in all() {
        statuses.extend(detect_one(cx, provider, &mut asked).await);
    }
    statuses
}

/// `provider`'s status as [`detect`] gives it, checked alone, Vagrant asked whether it is
/// installed (`vagrant --version`): `None` when it isn't offered on the context's host.
pub async fn status(cx: &Context, provider: Provider) -> Option<ProviderStatus> {
    let vagrant_installed = cx.read_ok("vagrant", &["--version"]).await.is_ok();
    let mut asked = Asked::of(cx, vagrant_installed).await;
    detect_one(cx, provider, &mut asked).await
}

/// What detection asks of the machine once for every provider.
struct Asked {
    /// Whether Vagrant is installed.
    vagrant_installed: bool,
    /// The Vagrant plugins installed, each with its version when it says one; why they can't be
    /// listed when `vagrant plugin list` fails (not "none": a plugin that is there would read as
    /// missing). None listed without Vagrant.
    plugins: Result<Vec<(String, Option<String>)>, String>,
    /// The groups this account joined since logging in, once a provider asked.
    pending_groups: Option<Vec<String>>,
}

impl Asked {
    /// The plugins asked of Vagrant when it is installed.
    async fn of(cx: &Context, vagrant_installed: bool) -> Asked {
        let plugins = if vagrant_installed {
            cx.read_ok("vagrant", &["plugin", "list", "--machine-readable"])
                .await
                .map(|out| isothrum_runner::vagrant_plugins(&out))
        } else {
            Ok(Vec::new())
        };
        Asked {
            vagrant_installed,
            plugins,
            pending_groups: None,
        }
    }

    /// The groups this account joined since logging in, asked the first time.
    async fn pending_groups(&mut self, cx: &Context) -> &[String] {
        if self.pending_groups.is_none() {
            self.pending_groups = Some(kvm::groups_pending(cx).await);
        }
        self.pending_groups.as_deref().unwrap_or_default()
    }

    /// `plugin`'s entry (its name and version, when it says one), when it is listed.
    fn plugin(&self, plugin: &str) -> Option<&(String, Option<String>)> {
        let listed = self.plugins.as_deref().unwrap_or_default();
        listed.iter().find(|(name, _)| name == plugin)
    }
}

/// One provider's status: `None` when it isn't offered on the context's host.
async fn detect_one(cx: &Context, provider: Provider, asked: &mut Asked) -> Option<ProviderStatus> {
    if !applicable(provider, &cx.host) {
        return None;
    }
    let found = hypervisor_found(cx, provider).await;
    if !offered(provider, &cx.host, found.installed == Some(true)) {
        return None;
    }
    let findings = findings(cx, provider, found, asked).await;
    Some(status_from(provider, asked.vagrant_installed, &findings))
}

/// What probing a hypervisor found.
struct Found {
    /// Whether it is installed, `None` when there is nothing local to probe.
    installed: Option<bool>,
    /// What its tool printed, standard output then error, when it has one and it answered.
    said: String,
}

/// Whether `provider`'s hypervisor is installed on the context's host: its tool starts (or ran
/// out of the probe's time: there, and slow), its app bundle or Windows feature is there.
async fn hypervisor_found(cx: &Context, provider: Provider) -> Found {
    let devices = &cx.host.devices;
    let installed = |installed| Found {
        installed: Some(installed),
        said: String::new(),
    };
    match ProviderRow::of(provider).check {
        Check::UtmApp => installed(devices.utm_app.exists()),
        Check::HypervFeature => installed(devices.hyperv_service.is_file()),
        Check::Remote => Found {
            installed: None,
            said: String::new(),
        },
        Check::Tool(..) | Check::Qemu => {
            let Some((program, args)) = probe_of(provider, cx.host.arch) else {
                return installed(false);
            };
            match cx.probe(program, args).await {
                Ok(captured) => Found {
                    installed: Some(true),
                    said: exec::all_lines(&captured),
                },
                Err(unanswered) => installed(unanswered.tool_is_there()),
            }
        }
    }
}

/// What the account lacks, as detection found it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Account {
    /// The right to manage Hyper-V.
    HypervAdmin,
    /// A new login, for the group it joined.
    NewLogin(String),
}

/// What detection found out about one provider beyond its hypervisor: the facts
/// [`unavailable_reason`] reads to name the first layer missing. Every probe is [`findings`]',
/// so the reason is decided without one.
#[derive(Debug, Clone, Default)]
struct Findings {
    /// KVM holds the CPU's virtualization (VirtualBox only): said before anything else.
    held: Option<Unavailable>,
    /// Whether the hypervisor's background service is wedged (a probe call timed out).
    unresponsive: bool,
    /// Whether the hypervisor is installed; `None` when there is nothing local to probe.
    hypervisor: Option<bool>,
    /// QEMU is there but not the `mkisofs` its VMs need ([`needs_mkisofs`]), so `hypervisor`
    /// reads false.
    mkisofs_missing: bool,
    /// The program whose absence says the hypervisor isn't installed, when one does.
    program: Option<&'static str>,
    /// The hypervisor's version, when it is installed and held to one (UTM).
    hypervisor_version: Option<String>,
    /// The Vagrant plugin it needs, if it is not built in.
    plugin: Option<&'static str>,
    /// That plugin's installed version, when it is listed with one.
    plugin_version: Option<String>,
    /// Whether that plugin is installed, at a version that works.
    plugin_installed: bool,
    /// Why the plugins couldn't be listed, when `vagrant plugin list` failed.
    plugins_unknown: Option<String>,
    /// Where the Vagrant VMware Utility stands; `None` when the provider doesn't use it.
    utility: Option<UtilityState>,
    /// What this account lacks to use it, if anything.
    account: Option<Account>,
    /// What keeps the installed hypervisor from starting a VM on this machine: KVM's device for
    /// libvirt and QEMU on Linux, Windows' hypervisor for Hyper-V.
    platform: Option<Unavailable>,
    /// VirtualBox's kernel driver not loaded: said last, once nothing else is missing.
    driver: Option<Unavailable>,
}

/// Everything detection asks of the machine about `provider`, whose hypervisor probe `found`.
async fn findings(cx: &Context, provider: Provider, found: Found, asked: &mut Asked) -> Findings {
    let host = &cx.host;
    let mkisofs_missing = found.installed == Some(true) && needs_mkisofs(provider, host.os) && !cx.tool_starts("mkisofs", &["-version"]).await;
    let hypervisor = if mkisofs_missing { Some(false) } else { found.installed };
    let ready = hypervisor == Some(true);
    let virtualbox = provider == Provider::Virtualbox;
    let plugin = provider.plugin();
    let listed = plugin.and_then(|p| asked.plugin(p));
    let plugin_version = listed.and_then(|(_, version)| version.clone());
    Findings {
        held: (virtualbox && kvm::holds_virtualization(host)).then_some(Unavailable::KvmHoldsVirtualization),
        unresponsive: asked.vagrant_installed && virtualbox && ready && vbox::wedged(cx).await,
        hypervisor,
        mkisofs_missing,
        program: probe_of(provider, host.arch).map(|(program, _)| program),
        hypervisor_version: if ready { hypervisor_version(cx, provider).await } else { None },
        plugin,
        plugin_installed: plugin.is_none() || (asked.vagrant_installed && listed.is_some() && plugin_too_old(provider, plugin_version.as_deref()).is_none()),
        plugin_version,
        plugins_unknown: asked.plugins.as_ref().err().cloned(),
        utility: if needs_vmware_utility(provider) {
            Some(vmware_utility(host).await)
        } else {
            None
        },
        account: if ready { account_lacks(cx, provider, asked).await } else { None },
        platform: if ready { platform_problem(cx, provider).await } else { None },
        driver: if virtualbox && ready {
            vbox::driver_problem(cx, &found.said).await
        } else {
            None
        },
    }
}

/// What this account lacks to use `provider`, installed: the right to manage Hyper-V (when the
/// runner's check says it may not; no answer adds nothing), or a new login after joining the
/// groups it is used through.
async fn account_lacks(cx: &Context, provider: Provider, asked: &mut Asked) -> Option<Account> {
    if provider == Provider::Hyperv && hyperv::needs_admin(cx).await {
        return Some(Account::HypervAdmin);
    }
    let joined = login_groups(provider, &cx.host);
    if joined.is_empty() {
        return None;
    }
    let pending = asked.pending_groups(cx).await;
    joined
        .iter()
        .any(|g| pending.contains(g))
        .then(|| Account::NewLogin(kvm::new_login_group(provider, &cx.host)))
}

/// What keeps `provider`'s installed hypervisor from starting a VM on the context's host: KVM's
/// device (libvirt, QEMU on Linux), Windows' hypervisor (Hyper-V). `None` when nothing does.
async fn platform_problem(cx: &Context, provider: Provider) -> Option<Unavailable> {
    if uses_kvm(provider, cx.host.os) {
        kvm::kvm_problem(&cx.host)
    } else if provider == Provider::Hyperv {
        hyperv::hypervisor_stopped(cx).await
    } else {
        None
    }
}

/// The hypervisor's own version, when it says one and is held to one: UTM's app bundle
/// (`CFBundleShortVersionString`).
async fn hypervisor_version(cx: &Context, provider: Provider) -> Option<String> {
    if ProviderRow::of(provider).check != Check::UtmApp {
        return None;
    }
    let plist = cx.host.devices.utm_app.join("Contents").join("Info.plist");
    let out = cx
        .probe("defaults", &["read", &plist.to_string_lossy(), "CFBundleShortVersionString"])
        .await
        .ok()?;
    let version = out.stdout.join("\n").trim().to_owned();
    (!version.is_empty()).then_some(version)
}

/// The status `provider` gets from what was found of it, given whether Vagrant is installed.
fn status_from(provider: Provider, vagrant_installed: bool, findings: &Findings) -> ProviderStatus {
    let needs = needs(vagrant_installed, findings.plugin_installed, findings.utility);
    let reason = unavailable_reason(provider, &needs, findings);
    ProviderStatus {
        provider,
        remote: ProviderRow::of(provider).remote(),
        available: reason.is_none(),
        hypervisor: findings.hypervisor,
        plugin: findings.plugin.map(str::to_owned),
        plugin_installed: findings.plugin_installed,
        utility: findings.utility,
        needs,
        reason,
        account_lacks: findings.account.as_ref().map(|account| match account {
            Account::HypervAdmin => AccountLacks::HypervAdmin,
            Account::NewLogin(_) => AccountLacks::NewLogin,
        }),
    }
}

/// Why `provider` can't run VMs here (`None`: it can), from what is still to install for it
/// (`needs`, which its status carries too, [`status_from`]) and what detection found: KVM holding the CPU's
/// virtualization first (nothing a setup installs mends it), then Vagrant, then the hypervisor
/// and the account, the machine's virtualization, the versions, the plugin and the VMware
/// utility, and VirtualBox's kernel driver last. It is `None` only when nothing is needed, so
/// readiness and a setup's steps can't disagree.
fn unavailable_reason(provider: Provider, needs: &[Need], f: &Findings) -> Option<Unavailable> {
    f.held
        .clone()
        .or_else(|| needs.contains(&Need::Vagrant).then_some(Unavailable::VagrantMissing))
        .or_else(|| hypervisor_reason(provider, f))
        .or_else(|| f.platform.clone())
        .or_else(|| version_reason(provider, f))
        .or_else(|| plugin_reason(needs, f))
        .or_else(|| f.driver.clone())
}

/// Why the hypervisor itself, or this account, keeps `provider` from running VMs.
fn hypervisor_reason(provider: Provider, f: &Findings) -> Option<Unavailable> {
    if f.unresponsive {
        Some(Unavailable::VirtualboxStuck)
    } else if f.hypervisor == Some(false) && provider == Provider::Hyperv {
        Some(Unavailable::HypervOff)
    } else if f.account == Some(Account::HypervAdmin) {
        Some(Unavailable::HypervNeedsAdmin)
    } else if f.mkisofs_missing {
        Some(Unavailable::MkisofsMissing)
    } else if f.hypervisor == Some(false) {
        Some(Unavailable::NotInstalled {
            program: f.program.map(str::to_owned),
        })
    } else if let Some(Account::NewLogin(group)) = &f.account {
        Some(Unavailable::NewLogin { group: group.clone() })
    } else {
        None
    }
}

/// Which of `provider`'s hypervisor (said first: updating the plugin wouldn't help) and plugin
/// is older than it is held to.
fn version_reason(provider: Provider, f: &Findings) -> Option<Unavailable> {
    if let Some((name, version, min)) = hypervisor_too_old(provider, f.hypervisor_version.as_deref()) {
        return Some(Unavailable::HypervisorTooOld {
            name: name.to_owned(),
            version: version.to_owned(),
            min: min.to_owned(),
        });
    }
    let (version, min) = plugin_too_old(provider, f.plugin_version.as_deref())?;
    Some(Unavailable::PluginTooOld {
        plugin: f.plugin.unwrap_or_default().to_owned(),
        version: version.to_owned(),
        min: min.to_owned(),
    })
}

/// Why the provider's plugin layer isn't ready: its plugins couldn't be listed, else the first
/// of its `needs`.
fn plugin_reason(needs: &[Need], f: &Findings) -> Option<Unavailable> {
    let plugin = || f.plugin.unwrap_or_default().to_owned();
    if let (Some(_), Some(why)) = (f.plugin, &f.plugins_unknown) {
        return Some(Unavailable::PluginsUnknown {
            plugin: plugin(),
            reason: why.clone(),
        });
    }
    needs.first().map(|need| match need {
        Need::Vagrant => Unavailable::VagrantMissing,
        Need::Plugin => Unavailable::PluginMissing { plugin: plugin() },
        Need::VmwareUtility if f.utility == Some(UtilityState::Stopped) => Unavailable::VmwareUtilityStopped,
        Need::VmwareUtility => Unavailable::VmwareUtilityMissing,
    })
}

/// The version `installed` of `provider`'s plugin when it is older than the one that works
/// ([`ProviderRow::plugin_minimum`]), with that minimum; `None` when it is recent enough, unknown,
/// or the plugin has no minimum.
fn plugin_too_old(provider: Provider, installed: Option<&str>) -> Option<(&str, &'static str)> {
    let (version, min) = installed.zip(ProviderRow::of(provider).plugin_minimum)?;
    older(version, min).then_some((version, min))
}

/// The version `installed` of `provider`'s hypervisor when it is older than the one its plugin
/// drives ([`ProviderRow::hypervisor_minimum`]), with the hypervisor's name and that minimum.
fn hypervisor_too_old(provider: Provider, installed: Option<&str>) -> Option<(&'static str, &str, &'static str)> {
    let (version, (name, min)) = installed.zip(ProviderRow::of(provider).hypervisor_minimum)?;
    older(version, min).then_some((name, version, min))
}

/// Whether the version `installed` says is before `min`, in Isoloom's order (a pre-release before
/// its release); not when either says no version.
fn older(installed: &str, min: &str) -> bool {
    isothrum_runner::version_found(installed)
        .zip(isothrum_runner::version_found(min))
        .is_some_and(|(found, min)| found < min)
}

#[cfg(test)]
mod tests;
