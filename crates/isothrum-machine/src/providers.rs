//! The Vagrant providers (hypervisors) this computer can drive, and detecting which are usable.
//! Vagrant abstracts the hypervisor; this detects what is installed here, what is still missing
//! for Vagrant to drive it ([`Need`]), and when it can't run VMs yet, why ([`Unavailable`], a code
//! a UI translates), and right before a start whether it is ready now ([`ready`]: UTM open and
//! answering, QEMU's emulator for a foreign CPU).
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
use crate::host::{Arch, Host, HostOs};

/// A Vagrant provider (the value given to `vagrant up --provider`).
pub use isothrum_runner::Provider;

mod hyperv;
pub(crate) mod kvm;
mod needs;
pub mod ready;
mod reasons;
mod unavailable;
mod vbox;
pub(crate) mod virtiofsd;
#[cfg(test)]
use reasons::hypervisor_too_old;
use reasons::{plugin_too_old, unavailable_reason};

pub use kvm::{groups_pending, kvm_group, login_groups, uses_kvm};
pub use needs::{Need, UtilityState, needs, vmware_utility};
pub use unavailable::Unavailable;
pub use virtiofsd::{needs_virtiofsd, virtiofsd};

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
    /// QEMU's system emulator for this machine's CPU ([`qemu_system`](crate::host::qemu_system)).
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
    pub oses: &'static [HostOs],
    /// Whether it is built for Windows on Arm.
    pub windows_on_arm: bool,
    /// The Mac CPU it is made for, when it is made for one: offered on the other only once
    /// installed ([`offered`]).
    pub mac_cpu: Option<Arch>,
    /// The Linux CPU its vendor ships it for, when it ships for one only: offered on the other
    /// only once installed ([`offered`]), as a setup can't install it there.
    pub linux_cpu: Option<Arch>,
    /// Where its vendor offers it, when a setup doesn't install it (behind a login, a licence, or
    /// a Windows feature the user turns on by hand).
    pub vendor: Option<&'static str>,
    /// The OSes where its VMs share a folder of this computer two-way.
    pub shares_folders: &'static [HostOs],
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
            oses: &crate::host::ALL_OSES,
            windows_on_arm: true,
            mac_cpu: None,
            linux_cpu: None,
            vendor: None,
            shares_folders: &crate::host::ALL_OSES,
            plugin_minimum: None,
            hypervisor_minimum: None,
            check: Check::Remote,
        };
        const fn found_by(check: Check) -> ProviderRow {
            ProviderRow { check, ..EVERYWHERE }
        }
        match provider {
            // Neither ships for Windows on Arm (no installer, no Arm VMs there). VirtualBox runs a
            // Mac's VMs on Intel only, and Oracle's Linux packages (its apt and rpm repositories)
            // are for x86-64 only.
            Provider::Virtualbox => ProviderRow {
                windows_on_arm: false,
                mac_cpu: Some(Arch::Amd64),
                linux_cpu: Some(Arch::Amd64),
                ..found_by(Check::Tool("VBoxManage", &["--version"]))
            },
            // VMware Workstation for Linux is built for x86-64 only.
            Provider::VmwareDesktop => ProviderRow {
                windows_on_arm: false,
                linux_cpu: Some(Arch::Amd64),
                vendor: Some("https://www.vmware.com/products/desktop-hypervisor/workstation-and-fusion"),
                ..found_by(Check::Tool("vmrun", &[]))
            },
            // Hyper-V's SMB folders ask for a password at every start.
            Provider::Hyperv => ProviderRow {
                oses: &[HostOs::Windows],
                vendor: Some("https://learn.microsoft.com/virtualization/hyper-v-on-windows/quick-start/enable-hyper-v"),
                shares_folders: &[],
                ..found_by(Check::HypervFeature)
            },
            Provider::Parallels => ProviderRow {
                oses: &[HostOs::Macos],
                vendor: Some("https://www.parallels.com/products/desktop/"),
                ..found_by(Check::Tool("prlctl", &["--version"]))
            },
            // UTM is the hypervisor made for Apple silicon.
            Provider::Utm => ProviderRow {
                oses: &[HostOs::Macos],
                mac_cpu: Some(Arch::Arm64),
                plugin_minimum: Some("0.2.0"),
                hypervisor_minimum: Some(("UTM", "4.6.5")),
                ..found_by(Check::UtmApp)
            },
            Provider::Libvirt => ProviderRow {
                oses: &[HostOs::Linux],
                ..found_by(Check::Tool("virsh", &["--version"]))
            },
            // vagrant-qemu builds a networked VM's cloud-init seed with `oscdimg` on Windows (the
            // Windows ADK's, which no setup brings), so it is offered on Linux and macOS only; it
            // shares a folder two-way through virtiofsd ([`needs_virtiofsd`]), on Linux only.
            Provider::Qemu => ProviderRow {
                oses: &[HostOs::Macos, HostOs::Linux],
                shares_folders: &[HostOs::Linux],
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

/// Whether `provider` is offered on `host` for VMs: applicable there, and built for its CPU: on a
/// Mac, the hypervisor made for its CPU (UTM on Apple silicon, VirtualBox on an Intel Mac); on
/// Linux, one its vendor ships for that CPU (VirtualBox and VMware Workstation on x86-64 only). One already `installed` is
/// offered wherever it applies: the user chose it.
#[must_use]
pub fn offered(provider: Provider, host: &Host, installed: bool) -> bool {
    let row = ProviderRow::of(provider);
    let cpu = match host.os {
        HostOs::Macos => row.mac_cpu,
        HostOs::Linux => row.linux_cpu,
        HostOs::Windows => None,
    };
    applicable(provider, host) && (installed || cpu.is_none_or(|cpu| cpu == host.arch))
}

/// Whether `provider`'s VMs need `mkisofs` on `os`: QEMU's on Linux. vagrant-qemu gives a VM on a
/// private network its settings in a cloud-init seed, an ISO Vagrant builds with `mkisofs` on Linux
/// (`hdiutil` on macOS, which every Mac has). Without it, `vagrant up` fails at the first VM.
#[must_use]
pub fn needs_mkisofs(provider: Provider, os: HostOs) -> bool {
    provider == Provider::Qemu && os == HostOs::Linux
}

/// Whether Vagrant drives `provider` through HashiCorp's Vagrant VMware Utility, a system service
/// of its own (installed apart from the plugin, by an administrator) that the
/// `vagrant-vmware-desktop` plugin asks to run VMware: without it every `vagrant up` fails.
#[must_use]
pub fn needs_vmware_utility(provider: Provider) -> bool {
    provider == Provider::VmwareDesktop
}

/// The local tool that proves `provider`'s hypervisor is installed on a CPU `arch`, if one does:
/// for a setup's install of it too ([`crate::install::present`]), so both read it alike.
#[must_use]
pub fn hypervisor_probe(provider: Provider, arch: Arch) -> Option<(&'static str, &'static [&'static str])> {
    match ProviderRow::of(provider).check {
        Check::Tool(program, args) => Some((program, args)),
        Check::Qemu => Some((crate::host::qemu_system(arch), &["--version"])),
        Check::UtmApp | Check::HypervFeature | Check::Remote => None,
    }
}

/// What the account lacks to use a provider installed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
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
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
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
    /// on; for QEMU on Linux, with the `mkisofs` its VMs need, [`needs_mkisofs`], and for libvirt
    /// and QEMU on Linux, with the virtiofsd they share folders through, [`needs_virtiofsd`]).
    /// `None` where nothing local is checked: a server.
    pub hypervisor: Option<bool>,
    /// The Vagrant plugin it needs, if it is not built in.
    pub plugin: Option<String>,
    /// Whether that plugin is installed at a version that works (true when the provider is built
    /// in): an older one reads as not installed, so a setup installs it again, which updates it.
    pub plugin_installed: bool,
    /// Where HashiCorp's Vagrant VMware Utility stands; `None` when the provider doesn't use it.
    pub utility: Option<UtilityState>,
    /// What is still to install for Vagrant to drive it here ([`needs()`]): empty when nothing is.
    pub needs: Vec<Need>,
    /// Why it can't run VMs yet, `None` when it can.
    pub reason: Option<Unavailable>,
    /// What this account lacks to use it although it is installed (the right to manage Hyper-V,
    /// a new login after joining libvirt's groups), whatever else is missing.
    #[cfg_attr(any(test, feature = "ts-rs"), ts(optional = nullable))]
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
            let Some((program, args)) = hypervisor_probe(provider, cx.host.arch) else {
                return installed(false);
            };
            let mut answer = cx.probe(program, args).await;
            // Not on PATH: where its app installs it (VMware's `vmrun`, Parallels' `prlctl`).
            if matches!(answer, Err(ref unanswered) if !unanswered.tool_is_there())
                && let Some(at) = devices.app_tool(program).into_iter().find(|t| t.is_file())
            {
                answer = cx.probe(&at.display().to_string(), args).await;
            }
            match answer {
                Ok(captured) => Found {
                    installed: Some(true),
                    said: exec::all_lines(&captured),
                },
                Err(unanswered) => installed(unanswered.tool_is_there()),
            }
        }
    }
}

/// A program a hypervisor's VMs need beside it, which its install brings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// `mkisofs`, for QEMU's VMs on Linux ([`needs_mkisofs`]).
    Mkisofs,
    /// A virtiofsd the hypervisor finds, for libvirt's and QEMU's shared folders on Linux
    /// ([`needs_virtiofsd`]).
    Virtiofsd,
}

/// The first program `provider`'s VMs need beside its hypervisor (installed) that the context's
/// host hasn't; `None` when it has them all.
async fn part_missing(cx: &Context, provider: Provider) -> Option<Part> {
    let host = &cx.host;
    if needs_mkisofs(provider, host.os) && !cx.tool_starts("mkisofs", &["-version"]).await {
        return Some(Part::Mkisofs);
    }
    (needs_virtiofsd(provider, host.os) && virtiofsd(provider, &host.devices).is_none()).then_some(Part::Virtiofsd)
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
    /// A program the hypervisor's VMs need beside it, which it is there without, so `hypervisor`
    /// reads false.
    part_missing: Option<Part>,
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
    let part_missing = if found.installed == Some(true) {
        part_missing(cx, provider).await
    } else {
        None
    };
    let hypervisor = if part_missing.is_some() { Some(false) } else { found.installed };
    let ready = hypervisor == Some(true);
    let virtualbox = provider == Provider::Virtualbox;
    let plugin = provider.plugin();
    let listed = plugin.and_then(|p| asked.plugin(p));
    let plugin_version = listed.and_then(|(_, version)| version.clone());
    Findings {
        held: (virtualbox && kvm::holds_virtualization(host)).then(|| {
            if kvm::shares_from_next_load(host) {
                Unavailable::KvmSharesAfterRestart
            } else {
                Unavailable::KvmHoldsVirtualization
            }
        }),
        unresponsive: asked.vagrant_installed && virtualbox && ready && vbox::wedged(cx).await,
        hypervisor,
        part_missing,
        program: hypervisor_probe(provider, host.arch).map(|(program, _)| program),
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

#[cfg(test)]
mod tests;
