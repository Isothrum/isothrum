//! A tool's install for real, as a setup runs it: the plan this computer's OS and package manager
//! make ([`install::macos::install`], [`install::linux::install`], [`install::windows::install`]),
//! each step run through the OS's own trusted flows, then the tool found installed and doing its
//! job ([`usable`]): a hypervisor read installed by its provider's detection, Docker's daemon
//! answering, Vagrant loading its plugins, Terraform validating a configuration, a cloud CLI
//! reading its configuration, VMware's utility answering. What the install reports it added is only
//! what was missing before it: a QEMU or libvirt installed by hand, run again for the programs its
//! VMs need, added those programs' packages alone. Each tool a setup installs has its test here:
//! one per cell of the workspace's install matrix.
//!
//! KVM is set to share the CPU's virtualization with VirtualBox the same way, where it holds it
//! (`share-virtualization` in `ISOTHRUM_E2E_INSTALL`), then VirtualBox's self-test boots a VM.
//!
//! A provider's Vagrant plugin is installed the same way, as a setup's step does it
//! ([`install::vagrant_plugin`]: for this account, no administrator), then read installed by the
//! provider's detection.
//!
//! A Windows feature starts at the restart after it is turned on (WSL 2, Hyper-V; Docker Desktop
//! runs on WSL 2, which its installer turns on): its test then asks for the restart
//! ([`restart!`]), and once resumed after it ([`resumed`]) checks what its install turned on.
//!
//! An install changes the computer for good, so it only runs where it is asked for:
//! `ISOTHRUM_E2E_INSTALL` names the tools to install (`docker,vagrant`; a plugin by its name,
//! `vagrant-qemu`; or `all`: those a setup offers here, a Mac's hypervisor the one made for its
//! CPU), on a clean
//! machine with its owner there for the administrator prompts and the installers' windows. A
//! tool already installed skips, and so does one missing that isn't asked for. What is installed
//! stays: the tests after these run on it.

use std::time::{Duration, Instant};

use isothrum_machine::docker::{self, Engine, StartFailed};
use isothrum_machine::host::os_name;
use isothrum_machine::install::{self, Dependency, Elevation, linux};
use isothrum_machine::providers::{self, Provider, Unavailable};
use isothrum_machine::setup::{self, Action, Machine, SetupPath, Wanted};
use isothrum_machine::wsl::{Marks, WslState};
use isothrum_machine::{Context, HostOs, check, wsl};

use crate::{RELOGIN, answer, as_started, here, reread, resumed, setting};

/// How long an install may take to be found once its steps are done (a Windows installer that
/// hands on to another process).
const FINISH: Duration = Duration::from_secs(30 * 60);
/// How often the tool is looked for meanwhile.
const LOOK_EVERY: Duration = Duration::from_secs(10);

/// How `ISOTHRUM_E2E_INSTALL` asks for `dep`'s install: by its name as the setup's table spells
/// it, or among `all` (the tools a setup offers here); not at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked {
    /// By its name.
    ByName,
    /// Among `all`.
    AmongAll,
    /// Not at all.
    No,
}

/// How `ISOTHRUM_E2E_INSTALL` asks for `dep`'s install.
fn asked(dep: Dependency) -> Asked {
    asked_by(&serde_json::to_value(dep).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default())
}

/// How `ISOTHRUM_E2E_INSTALL` asks for the install of what it calls `name`.
fn asked_by(name: &str) -> Asked {
    let list = setting("ISOTHRUM_E2E_INSTALL").unwrap_or_default();
    let named: Vec<&str> = list.split(',').map(str::trim).collect();
    if named.contains(&name) {
        Asked::ByName
    } else if named.contains(&"all") {
        Asked::AmongAll
    } else {
        Asked::No
    }
}

/// How a root step is elevated here: through the OS's prompt, as a setup on a desktop does, where
/// a Linux desktop session answers pkexec's prompt or `ISOTHRUM_E2E_ELEVATION=pkexec` says polkit
/// lets this user through without one (a test VM's rule, never a person's computer); else
/// unattended (`sudo -n`, or as is when root). A Mac's run, over ssh, has no session that could
/// show macOS's administrator prompt: unattended too, `ISOTHRUM_E2E_ELEVATION=prompt` asking for
/// the prompt where its owner answers it at the Mac.
async fn elevation(cx: &Context) -> Elevation {
    let desktop = setting("DISPLAY").is_some() || setting("WAYLAND_DISPLAY").is_some();
    if desktop || matches!(setting("ISOTHRUM_E2E_ELEVATION").as_deref(), Some("pkexec" | "prompt")) {
        return Elevation::Prompt;
    }
    let root = answer(cx, "id", &["-u"]).await.is_some_and(|uid| uid.trim() == "0");
    Elevation::Unattended { root }
}

/// The install of `dep` on this computer, as its setup plans it.
async fn plan(cx: &Context, dep: Dependency) -> Result<Vec<install::Step>, install::Refusal> {
    match cx.host.os {
        HostOs::Macos => install::macos::install(dep, cx.host.packages, cx.host.arch, elevation(cx).await),
        HostOs::Windows => install::windows::install(dep, cx.host.arch),
        HostOs::Linux => {
            let root = linux::Root {
                elevation: elevation(cx).await,
                proxy: linux::proxy_assignments(|name| std::env::var(name).ok()),
            };
            install::linux::install(dep, cx.host.packages, cx.host.arch, setting("USER").as_deref(), &root)
        }
    }
}

/// The hypervisor `dep` is for: its own, or the one it serves ([`Dependency::serves`]: VMware's
/// for VMware's utility).
fn serves(dep: Dependency) -> Option<providers::Provider> {
    dep.serves().or_else(|| dep.hypervisor())
}

/// Hyper-V's Windows feature's install state (1: on), read without an administrator.
const HYPERV_FEATURE: &str = "(Get-CimInstance Win32_OptionalFeature | Where-Object Name -eq 'Microsoft-Hyper-V').InstallState";

/// Whether `dep` is installed here: a Windows feature, which no version probe tells, once it is
/// turned on (also before the restart that starts it), as WSL's check and Windows' own feature
/// list read it; any other tool as a setup reads it ([`install::present`]). `None`: it can't be
/// told.
async fn found(cx: &Context, dep: Dependency) -> Option<bool> {
    match dep {
        Dependency::Wsl => match wsl::check(cx, Marks::default()).await?.state {
            WslState::Ready | WslState::NeedsReboot => Some(true),
            WslState::Missing => Some(false),
            WslState::Unknown => None,
        },
        Dependency::Hyperv => answer(cx, "powershell", &["-NoProfile", "-NonInteractive", "-Command", HYPERV_FEATURE])
            .await
            .map(|state| state.trim() == "1"),
        _ => install::present(cx, dep).await,
    }
}

/// Why the computer must restart before `dep`, just installed here, can be used, when it must:
/// WSL 2 turned on starts at the next restart, and with it Docker Desktop, which runs on it; so
/// does Hyper-V, until then not read as installed by its provider's detection.
async fn waits_for_restart(cx: &Context, dep: Dependency) -> Option<String> {
    if cx.host.os != HostOs::Windows {
        return None;
    }
    match dep {
        Dependency::Wsl | Dependency::Docker => {
            let just_installed = Marks {
                installed_this_boot: true,
                restarted_for_wsl: false,
            };
            let state = wsl::check(cx, just_installed).await.map(|w| w.state);
            (state == Some(WslState::NeedsReboot)).then(|| format!("{dep:?} runs once WSL 2, turned on, starts at the next restart"))
        }
        Dependency::Hyperv => {
            let read = providers::status(cx, Provider::Hyperv).await.and_then(|s| s.hypervisor);
            (read != Some(true)).then(|| "Hyper-V, turned on, starts at the next restart".to_owned())
        }
        _ => None,
    }
}

/// Why `dep`'s install isn't tested here, when it isn't: it doesn't run here, it is installed
/// already (or that can't be told), or it isn't asked for.
async fn not_tested(cx: &Context, dep: Dependency) -> Option<String> {
    if !dep.runs_on(cx.host.os) {
        return Some(format!("{dep:?} doesn't run on {}", os_name(cx.host.os)));
    }
    if let Some(why) = dep.no_build(cx.host.os, cx.host.arch) {
        return Some(format!("{dep:?} isn't offered on {} {:?}: {why}", os_name(cx.host.os), cx.host.arch));
    }
    // A tool that serves a hypervisor (VMware's utility serves VMware: its Windows installer
    // refuses to run without Workstation, its Linux service doesn't answer) is installed once
    // that hypervisor is, never before: a setup plans it after it.
    if let Some(p) = dep.serves()
        && providers::status(cx, p).await.and_then(|s| s.hypervisor) != Some(true)
    {
        return Some(format!(
            "{dep:?} serves {}, which isn't installed here (its vendor's download): a setup installs it once {} is",
            p.id(),
            p.id()
        ));
    }
    match found(cx, dep).await {
        Some(true) => Some(format!("{dep:?} is installed here already: an install is tested on a machine without it")),
        None => Some(format!("whether {dep:?} is installed can't be told here")),
        Some(false) => match asked(dep) {
            Asked::No => Some(format!(
                "{dep:?} is missing here; ISOTHRUM_E2E_INSTALL doesn't ask for its install (it changes this computer for good)"
            )),
            // A setup offers a Mac the hypervisor made for its CPU only (UTM on Apple silicon,
            // VirtualBox on Intel): `all` installs what it offers, a name installs that tool.
            Asked::AmongAll if serves(dep).is_some_and(|p| !providers::offered(p, &cx.host, false)) => Some(format!(
                "{dep:?} isn't offered on this computer's CPU ({:?}); ISOTHRUM_E2E_INSTALL names it to install it anyway",
                cx.host.arch
            )),
            Asked::AmongAll | Asked::ByName => None,
        },
    }
}

/// Runs `dep`'s install, then waits until it is found installed (up to [`FINISH`]), the `PATH` read
/// again each time. A Mac's app installed from its vendor's disk image is whole and signed then
/// ([`app_verifies`]): nothing is left for anyone to drag or finish.
/// Then what it added ([`install::Before::installed`]) is only what was missing before it
/// ([`added_only_what_was_missing`]).
async fn install(cx: &Context, dep: Dependency) {
    let steps = plan(cx, dep).await.unwrap_or_else(|refused| panic!("no install of {dep:?} here: {refused}"));
    let there = install::there(cx, dep).await;
    let itself = hypervisor_itself_there(cx, dep).await;
    let before = install::before(cx, dep).await;
    for s in &steps {
        println!("$ {}", install::command_line(s));
    }
    install::run_steps(cx, &steps, &mut |line| println!("{line}"))
        .await
        .unwrap_or_else(|failed| panic!("{dep:?}'s install failed: {failed:?}"));
    app_verifies(cx, dep).await;
    let start = Instant::now();
    while found(&reread(cx).await, dep).await != Some(true) {
        assert!(
            start.elapsed() < FINISH,
            "{dep:?} isn't found installed {} min after its install",
            FINISH.as_secs() / 60
        );
        println!("waiting for {dep:?}'s installer to finish…");
        tokio::time::sleep(LOOK_EVERY).await;
    }
    let installed = before.installed(&reread(cx).await).await;
    println!("what {dep:?}'s install added: {installed:#?}");
    added_only_what_was_missing(cx, dep, there, itself, &installed);
}

/// Whether `dep` is a hypervisor whose program answers here (its helpers aside): one installed by
/// hand, say, whose install a setup runs again for the programs its VMs need beside it.
async fn hypervisor_itself_there(cx: &Context, dep: Dependency) -> bool {
    let probe = dep.hypervisor().and_then(|p| providers::hypervisor_probe(p, cx.host.arch));
    match probe {
        Some((program, args)) => answer(cx, program, args).await.is_some(),
        None => false,
    }
}

/// What `dep`'s install added (`installed`) is only what was missing before it: none of the items
/// `there` before, and, where its hypervisor `itself` was there already (installed by hand, its
/// helpers missing), only its helpers' packages ([`providers::helpers`]), never the hypervisor's:
/// a clean-up removes what an install added, and must never remove what was there before.
fn added_only_what_was_missing(
    cx: &Context,
    dep: Dependency,
    there: Option<std::collections::BTreeSet<install::Item>>,
    itself: bool,
    installed: &install::Installed,
) {
    // A Windows feature (WSL 2, Hyper-V) is turned off by its user in Windows Features, never
    // removed by a setup (`Refusal::WindowsFeature`): its install leaves no record to hold.
    if matches!(install::windows::uninstall(dep), Err(install::Refusal::WindowsFeature { .. })) {
        println!("{dep:?} is a Windows feature, which a setup never removes: its install records nothing");
        return;
    }
    let there = there.unwrap_or_else(|| panic!("what of {dep:?}'s install is here can't be told"));
    let before: Vec<&install::Item> = installed.added.iter().filter(|item| there.contains(*item)).collect();
    assert!(before.is_empty(), "{dep:?}'s install claims {before:?}, which were there before it");
    assert!(!installed.added.is_empty(), "{dep:?}'s install added nothing, though it was missing");
    if itself {
        let manager = cx.host.packages.and_then(isothrum_machine::Packages::linux).expect("a Linux package manager");
        let provider = dep.hypervisor().expect("a hypervisor");
        let helpers: Vec<install::Item> = providers::helpers(provider, cx.host.os)
            .iter()
            .map(|h| install::Item::Package(h.package(manager).to_owned()))
            .collect();
        let more: Vec<&install::Item> = installed.added.iter().filter(|item| !helpers.contains(item)).collect();
        assert!(
            more.is_empty(),
            "{} was there before: its install added only its helpers, not {more:?}",
            provider.id()
        );
        println!("{} was there before: its install added only {:?}", provider.id(), installed.added);
    }
}

/// A Mac's app installed from its vendor's disk image or package, right after its install: there,
/// whole and signed (`codesign --verify --deep --strict`), accepted by Gatekeeper (`spctl`), and
/// without Finder's marks of a copy under way.
async fn app_verifies(cx: &Context, dep: Dependency) {
    let Some(app) = dep.row().mac_app.filter(|_| cx.host.os == HostOs::Macos) else {
        return;
    };
    works(cx, "codesign", &["--verify", "--deep", "--strict", "--verbose=2", app]).await;
    works(cx, "spctl", &["--assess", "--type", "execute", "--verbose", app]).await;
    let attributes = works(cx, "xattr", &[app]).await;
    assert!(
        !attributes.contains("ResumableCopy") && !attributes.contains("com.apple.finder.copy"),
        "{app} carries a copy's marks: {attributes}"
    );
    println!("{app} is whole and signed");
}

/// `program` with `args`, which must succeed: what it printed.
async fn works(cx: &Context, program: &str, args: &[&str]) -> String {
    let said = answer(cx, program, args).await;
    said.unwrap_or_else(|| panic!("`{program} {}` fails once installed", args.join(" ")))
}

/// What uses `dep` once installed reads it so: a hypervisor its provider's detection, with each
/// program its VMs need beside it ([`providers::helpers`]: QEMU's `mkisofs` and virtiofsd on
/// Linux) found as the hypervisor finds it, Docker its engine, started (the only engine a test
/// starts: this one installed it); each other tool doing what a run asks of it first
/// ([`works_as_a_run_needs`]), so a tool that is there but broken fails its cell.
async fn usable(cx: &Context, dep: Dependency) {
    works_as_a_run_needs(cx, dep).await;
    if dep == Dependency::Wsl {
        let restarted = Marks {
            installed_this_boot: false,
            restarted_for_wsl: true,
        };
        let check = wsl::check(cx, restarted).await.expect("WSL is Windows'");
        println!("{check:#?}");
        assert_eq!(check.state, WslState::Ready, "WSL 2 reads ready");
    }
    if let Some(provider) = dep.hypervisor() {
        for &helper in providers::helpers(provider, cx.host.os) {
            assert!(
                helper.found(cx, provider).await,
                "{} finds {} after its install",
                provider.id(),
                helper.program()
            );
            println!("{} finds {}", provider.id(), helper.program());
        }
        let status = providers::status(cx, provider).await.expect("offered here");
        println!("{status:#?}");
        assert_eq!(status.hypervisor, Some(true), "{} reads it installed", provider.id());
    }
    if dep == Dependency::Docker {
        docker_answers(cx).await;
    }
}

/// Docker's engine started, its daemon answering this account, or, on Linux right after the
/// install added the account to the docker group, its next login.
async fn docker_answers(cx: &Context) {
    let engine = if cx.host.os == HostOs::Linux { Engine::Native } else { Engine::DockerDesktop };
    match docker::start_engine(cx, engine, elevation(cx).await, &mut |line| println!("{line}")).await {
        Ok(()) => {
            docker::daemon_os(cx).await.expect("its daemon answers");
        }
        // Linux: the install added this account to the docker group, which applies from its
        // next login, as the install said; until then the daemon refuses this session. It
        // answers that next login: a shell with the group, as one would have.
        Err(StartFailed::Denied(said)) if providers::groups_pending(cx).await.iter().any(|g| g == "docker") => {
            println!("the daemon refuses this session until its next login ({said}): asked as that login");
            let os = answer(cx, "sg", &["docker", "-c", "docker version --format '{{.Server.Os}}'"]).await;
            assert_eq!(os.as_deref().map(str::trim), Some("linux"), "its daemon answers the next login");
        }
        Err(failed) => panic!("the engine installed starts: {failed:?}"),
    }
}

/// A tool other than a hypervisor or Docker doing what a run asks of it first.
async fn works_as_a_run_needs(cx: &Context, dep: Dependency) {
    match dep {
        // Its Ruby and gems load: what every `vagrant up` needs first.
        Dependency::Vagrant => {
            println!("{}", works(cx, "vagrant", &["plugin", "list"]).await);
        }
        // It reads and validates a configuration, as a run's `init` and `plan` do first.
        Dependency::Terraform => {
            let dir = tempfile::tempdir().expect("a folder");
            std::fs::write(dir.path().join("main.tf"), "output \"ok\" {\n  value = \"yes\"\n}\n").expect("a configuration");
            let at = format!("-chdir={}", dir.path().display());
            works(cx, "terraform", &[&at, "init", "-input=false", "-no-color"]).await;
            let valid = works(cx, "terraform", &[&at, "validate", "-json"]).await;
            assert!(valid.contains("\"valid\": true"), "terraform validates a configuration: {valid}");
        }
        // Each cloud CLI runs and reads its (empty) configuration: no account needed.
        Dependency::Awscli => {
            println!("{}", works(cx, "aws", &["configure", "list"]).await);
        }
        Dependency::Azurecli => {
            let version = works(cx, "az", &["version", "--output", "json"]).await;
            assert!(version.contains("\"azure-cli\""), "az says its version: {version}");
        }
        Dependency::Gcloud => {
            println!("{}", works(cx, "gcloud", &["config", "list", "--format=json"]).await);
        }
        // Its service answers, as VMware's Vagrant plugin needs.
        Dependency::VmwareUtility => {
            let state = providers::vmware_utility(&cx.host).await;
            assert_eq!(state, providers::UtilityState::Running, "the VMware utility's service answers");
        }
        _ => {}
    }
}

/// The groups using `dep` takes on this computer (Linux): Docker's `docker`, a hypervisor's own
/// ([`providers::login_groups`]: libvirt's, `/dev/kvm`'s).
fn groups_of(cx: &Context, dep: Dependency) -> Vec<String> {
    match dep {
        Dependency::Docker => vec!["docker".to_owned()],
        _ => dep.hypervisor().map(|p| providers::login_groups(p, &cx.host)).unwrap_or_default(),
    }
}

/// What a setup for `dep` plans on this computer as it is now: Docker's engine, or `dep`'s
/// hypervisor and what Vagrant needs for it.
async fn setup_for(cx: &Context, dep: Dependency) -> setup::Plan {
    let found = check::check(cx, wsl::Marks::default()).await;
    let machine = Machine::of(&found, cx.host.clone());
    let options = setup::options(&machine, false);
    let wanted = match dep.hypervisor() {
        Some(p) => Wanted {
            paths: vec![SetupPath::Vm],
            hypervisors: vec![p],
            ..Wanted::default()
        },
        None => Wanted {
            paths: vec![SetupPath::Docker],
            ..Wanted::default()
        },
    };
    setup::plan(&machine, &options, &wanted)
}

/// On Linux, the groups `dep`'s install joined this account to apply from its next login: a
/// setup asks its user for that login ([`Action::Relogin`], last), and this run, as that user
/// does, asks for it before the tests that use them ([`RELOGIN`]).
async fn next_login(cx: &Context, dep: Dependency) {
    let wanted = groups_of(cx, dep);
    let pending: Vec<String> = providers::groups_pending(cx).await.into_iter().filter(|g| wanted.contains(g)).collect();
    if pending.is_empty() {
        return;
    }
    let plan = setup_for(cx, dep).await;
    println!("{plan:#?}");
    assert_eq!(
        plan.steps.last().map(|s| &s.action),
        Some(&Action::Relogin),
        "a setup for {dep:?} ends with a new login while {pending:?} apply from it"
    );
    println!("{RELOGIN}{dep:?} joined this account to {} (from its next login)", pending.join(", "));
}

/// Installs `dep` for real when it is missing here and asked for; skips, saying why, else.
async fn installs(dep: Dependency) {
    let cx = here().await;
    if !resumed() {
        if let Some(why) = not_tested(&cx, dep).await {
            skip!("{why}")
        }
        install(&cx, dep).await;
        if let Some(why) = waits_for_restart(&reread(&cx).await, dep).await {
            restart!("{why}")
        }
    }
    let cx = reread(&cx).await;
    usable(&cx, dep).await;
    next_login(&cx, dep).await;
}

// WSL 2 and Hyper-V first: Docker Desktop runs on WSL 2, and its installer turns both on (Hyper-V
// for its other backend), which would leave their own installs untested on a fresh machine.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn wsl_is_installed_where_it_is_missing() {
    installs(Dependency::Wsl).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn hyperv_is_installed_where_it_is_missing() {
    installs(Dependency::Hyperv).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn docker_is_installed_where_it_is_missing() {
    installs(Dependency::Docker).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn vagrant_is_installed_where_it_is_missing() {
    installs(Dependency::Vagrant).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn terraform_is_installed_where_it_is_missing() {
    installs(Dependency::Terraform).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn virtualbox_is_installed_where_it_is_missing() {
    installs(Dependency::Virtualbox).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn qemu_is_installed_where_it_is_missing() {
    installs(Dependency::Qemu).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn utm_is_installed_where_it_is_missing() {
    installs(Dependency::Utm).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn libvirt_is_installed_where_it_is_missing() {
    installs(Dependency::Libvirt).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn awscli_is_installed_where_it_is_missing() {
    installs(Dependency::Awscli).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn azurecli_is_installed_where_it_is_missing() {
    installs(Dependency::Azurecli).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn gcloud_is_installed_where_it_is_missing() {
    installs(Dependency::Gcloud).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn vmware_utility_is_installed_where_it_is_missing() {
    installs(Dependency::VmwareUtility).await;
}

/// Lets VirtualBox share the CPU's virtualization with KVM for real, as a setup's step does
/// ([`setup::share_virtualization`]), where KVM holds it (Linux 6.12 and later) and
/// `ISOTHRUM_E2E_INSTALL` names `share-virtualization` (or `all`): KVM set to take it only while
/// one of its own VMs runs, which stays. Then VirtualBox reads usable and its self-test boots a VM.
/// Where VirtualBox then still waits for a restart, as a setup's pass ends for one
/// ([`Action::ends_pass`]: KVM was in use, or VirtualBox's driver was built for the newer kernel
/// its install brought), the computer restarts and this test carries on, resumed.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn virtualbox_shares_the_cpus_virtualization_with_kvm_where_kvm_holds_it() {
    let cx = here().await;
    if cx.host.os != HostOs::Linux {
        skip!("KVM is Linux's")
    }
    if !resumed() {
        let Some(before) = providers::status(&cx, Provider::Virtualbox).await else {
            skip!("VirtualBox isn't offered here")
        };
        if before.reason != Some(Unavailable::KvmHoldsVirtualization) {
            skip!("KVM doesn't hold the CPU's virtualization here: {:?}", before.reason)
        }
        if before.hypervisor != Some(true) {
            skip!("VirtualBox isn't installed here, which KVM would share the CPU's virtualization with")
        }
        if asked_by("share-virtualization") == Asked::No {
            skip!("KVM holds the CPU's virtualization; ISOTHRUM_E2E_INSTALL doesn't ask to let it share (it changes this computer for good)")
        }
        let shared = setup::share_virtualization(&cx, elevation(&cx).await, &mut |line| println!("{line}"))
            .await
            .unwrap_or_else(|failed| panic!("KVM wasn't set to share the CPU's virtualization: {failed:?}"));
        if shared == setup::Shared::AfterRestart {
            restart!("KVM shares the CPU's virtualization from the next restart (it was in use)")
        }
    }
    let after = providers::status(&reread(&cx).await, Provider::Virtualbox).await.expect("offered here");
    println!("{after:#?}");
    if let Some(reason) = &after.reason
        && !resumed()
        && (Action::Blocked { reason: reason.clone() }).ends_pass()
    {
        restart!("VirtualBox waits for a restart: {reason:?}")
    }
    let param = std::fs::read_to_string(&cx.host.devices.kvm_virt_at_load).unwrap_or_default();
    assert_eq!(param.trim(), "N", "KVM loaded with enable_virt_at_load off");
    assert_eq!(after.reason, None, "VirtualBox can run VMs");
    super::selftest::boots_on(Provider::Virtualbox).await;
}

/// Installs `provider`'s Vagrant plugin for real, as a setup's step does (for this account, no
/// administrator), when it is missing here and asked for, then reads it installed by the
/// provider's detection; skips, saying why, else.
async fn plugin_installs(provider: Provider) {
    let cx = here().await;
    let Some(before) = providers::status(&cx, provider).await else {
        skip!("{} isn't offered on {}", provider.id(), os_name(cx.host.os))
    };
    let plugin = before.plugin.clone().expect("a provider of a plugin");
    if before.plugin_installed {
        skip!("{plugin} is installed here already: an install is tested on a machine without it")
    }
    if install::present(&cx, Dependency::Vagrant).await != Some(true) {
        skip!("{plugin} is missing here, and so is Vagrant, which installs it")
    }
    if asked_by(&plugin) == Asked::No {
        skip!("{plugin} is missing here; ISOTHRUM_E2E_INSTALL doesn't ask for its install (it changes this computer for good)")
    }
    let step = install::vagrant_plugin(&plugin).unwrap_or_else(|| panic!("no install step for {plugin}"));
    println!("$ {}", install::command_line(&step));
    // Run as a setup in a process started before Vagrant's install runs it: the step finds the
    // Vagrant installed since by itself (on Windows, the PATH its installer wrote to the
    // registry), not through this test's re-read PATH.
    install::run_steps(&as_started().await, &[step], &mut |line| println!("{line}"))
        .await
        .unwrap_or_else(|failed| panic!("{plugin}'s install failed: {failed:?}"));
    let after = providers::status(&cx, provider).await.expect("offered here");
    println!("{after:#?}");
    assert!(after.plugin_installed, "{} reads {plugin} installed", provider.id());
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_vmware_desktop_is_installed_where_it_is_missing() {
    plugin_installs(Provider::VmwareDesktop).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_parallels_is_installed_where_it_is_missing() {
    plugin_installs(Provider::Parallels).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_libvirt_is_installed_where_it_is_missing() {
    plugin_installs(Provider::Libvirt).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_qemu_is_installed_where_it_is_missing() {
    plugin_installs(Provider::Qemu).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_utm_is_installed_where_it_is_missing() {
    plugin_installs(Provider::Utm).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn plugin_vagrant_vmware_esxi_is_installed_where_it_is_missing() {
    plugin_installs(Provider::VmwareEsxi).await;
}
