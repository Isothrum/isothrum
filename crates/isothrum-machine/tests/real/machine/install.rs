//! A tool's install for real, as a setup runs it: the plan this computer's OS and package manager
//! make ([`install::macos::install`], [`install::linux::install`], [`install::windows::install`]),
//! each step run through the OS's own trusted flows, then the tool found installed and doing its
//! job ([`usable`]): a hypervisor read installed by its provider's detection, Docker's daemon
//! answering, Vagrant loading its plugins, Terraform validating a configuration, a cloud CLI
//! reading its configuration, VMware's utility answering. Each tool a setup installs has its test
//! here: one per cell of the workspace's install matrix.
//!
//! A provider's Vagrant plugin is installed the same way, as a setup's step does it
//! ([`install::vagrant_plugin`]: for this account, no administrator), then read installed by the
//! provider's detection.
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
use isothrum_machine::providers::{self, Provider};
use isothrum_machine::{Context, HostOs};

use crate::{answer, here, reread, setting};

/// How long an installer its user finishes in its own window (macOS's casks) may take.
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

/// How a root step is elevated here: through pkexec, as a setup on a desktop does, where a desktop
/// session answers its prompt or `ISOTHRUM_E2E_ELEVATION=pkexec` says polkit lets this user
/// through without one (a test VM's rule, never a person's computer); else unattended (`sudo -n`,
/// or as is when root).
async fn elevation(cx: &Context) -> Elevation {
    let desktop = setting("DISPLAY").is_some() || setting("WAYLAND_DISPLAY").is_some();
    if desktop || setting("ISOTHRUM_E2E_ELEVATION").as_deref() == Some("pkexec") {
        return Elevation::Prompt;
    }
    let root = answer(cx, "id", &["-u"]).await.is_some_and(|uid| uid.trim() == "0");
    Elevation::Unattended { root }
}

/// The install of `dep` on this computer, as its setup plans it.
async fn plan(cx: &Context, dep: Dependency) -> Result<Vec<install::Step>, install::Refusal> {
    match cx.host.os {
        HostOs::Macos => install::macos::install(dep, cx.host.packages, cx.host.arch),
        HostOs::Windows => install::windows::install(dep, cx.host.arch),
        HostOs::Linux => {
            let root = linux::Root {
                elevation: elevation(cx).await,
                proxy: linux::proxy_assignments(|name| std::env::var(name).ok()),
            };
            install::linux::install(dep, cx.host.packages, setting("USER").as_deref(), &root)
        }
    }
}

/// The hypervisor `dep` is for: its own, or VMware's for VMware's utility (a setup installs it
/// where it offers VMware).
fn serves(dep: Dependency) -> Option<providers::Provider> {
    if dep == Dependency::VmwareUtility {
        return Some(providers::Provider::VmwareDesktop);
    }
    dep.hypervisor()
}

/// Why `dep`'s install isn't tested here, when it isn't: it doesn't run here, it is installed
/// already (or that can't be told), or it isn't asked for.
async fn not_tested(cx: &Context, dep: Dependency) -> Option<String> {
    if !dep.runs_on(cx.host.os) {
        return Some(format!("{dep:?} doesn't run on {}", os_name(cx.host.os)));
    }
    match install::present(cx, dep).await {
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

/// Runs `dep`'s install, then waits until it is found installed (an installer its user finishes
/// in its own window takes as long as they do, up to [`FINISH`]), the `PATH` read again each time.
async fn install(cx: &Context, dep: Dependency) {
    let steps = plan(cx, dep).await.unwrap_or_else(|refused| panic!("no install of {dep:?} here: {refused}"));
    for s in &steps {
        println!("$ {}", install::command_line(s));
    }
    install::run_steps(cx, &steps, &mut |line| println!("{line}"))
        .await
        .unwrap_or_else(|failed| panic!("{dep:?}'s install failed: {failed:?}"));
    let start = Instant::now();
    while install::present(&reread(cx).await, dep).await != Some(true) {
        assert!(
            start.elapsed() < FINISH,
            "{dep:?} isn't found installed {} min after its install",
            FINISH.as_secs() / 60
        );
        println!("waiting for {dep:?}'s installer to finish…");
        tokio::time::sleep(LOOK_EVERY).await;
    }
}

/// `program` with `args`, which must succeed: what it printed.
async fn works(cx: &Context, program: &str, args: &[&str]) -> String {
    let said = answer(cx, program, args).await;
    said.unwrap_or_else(|| panic!("`{program} {}` fails once installed", args.join(" ")))
}

/// What uses `dep` once installed reads it so: a hypervisor its provider's detection, Docker its
/// engine, started (the only engine a test starts: this one installed it); each other tool doing
/// what a run asks of it first ([`works_as_a_run_needs`]), so a tool that is there but broken
/// fails its cell.
async fn usable(cx: &Context, dep: Dependency) {
    works_as_a_run_needs(cx, dep).await;
    if let Some(provider) = dep.hypervisor() {
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

/// Installs `dep` for real when it is missing here and asked for; skips, saying why, else.
async fn installs(dep: Dependency) {
    let cx = here().await;
    if let Some(why) = not_tested(&cx, dep).await {
        skip!("{why}")
    }
    install(&cx, dep).await;
    usable(&reread(&cx).await, dep).await;
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
    install::run_steps(&cx, &[step], &mut |line| println!("{line}"))
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
