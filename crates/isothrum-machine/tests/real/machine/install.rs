//! A tool's install for real, as a setup runs it: the plan this computer's OS and package manager
//! make ([`install::macos::install`], [`install::linux::install`], [`install::windows::install`]),
//! each step run through the OS's own trusted flows, then the tool found installed, and a
//! hypervisor read installed by its provider's detection.
//!
//! An install changes the computer for good, so it only runs where it is asked for:
//! `ISOTHRUM_E2E_INSTALL` names the tools to install (`docker,vagrant`, or `all`: those a setup
//! offers here, a Mac's hypervisor the one made for its CPU), on a clean
//! machine with its owner there for the administrator prompts and the installers' windows. A
//! tool already installed skips, and so does one missing that isn't asked for. What is installed
//! stays: the tests after these run on it.

use std::time::{Duration, Instant};

use isothrum_machine::docker::{self, Engine};
use isothrum_machine::host::os_name;
use isothrum_machine::install::{self, Dependency, Elevation, linux};
use isothrum_machine::providers;
use isothrum_machine::{Context, HostOs};

use crate::{answer, here, setting};

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
    let name = serde_json::to_value(dep).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    let list = setting("ISOTHRUM_E2E_INSTALL").unwrap_or_default();
    let named: Vec<&str> = list.split(',').map(str::trim).collect();
    if named.contains(&name.as_str()) {
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
            Asked::AmongAll if dep.hypervisor().is_some_and(|p| !providers::offered(p, &cx.host, false)) => Some(format!(
                "{dep:?} isn't offered on this computer's CPU ({:?}); ISOTHRUM_E2E_INSTALL names it to install it anyway",
                cx.host.arch
            )),
            Asked::AmongAll | Asked::ByName => None,
        },
    }
}

/// Runs `dep`'s install, then waits until it is found installed (an installer its user finishes
/// in its own window takes as long as they do, up to [`FINISH`]).
async fn install(cx: &Context, dep: Dependency) {
    let steps = plan(cx, dep).await.unwrap_or_else(|refused| panic!("no install of {dep:?} here: {refused}"));
    for s in &steps {
        println!("$ {}", install::command_line(s));
    }
    install::run_steps(cx, &steps, &mut |line| println!("{line}"))
        .await
        .unwrap_or_else(|failed| panic!("{dep:?}'s install failed: {failed:?}"));
    let start = Instant::now();
    while install::present(cx, dep).await != Some(true) {
        assert!(
            start.elapsed() < FINISH,
            "{dep:?} isn't found installed {} min after its install",
            FINISH.as_secs() / 60
        );
        println!("waiting for {dep:?}'s installer to finish…");
        tokio::time::sleep(LOOK_EVERY).await;
    }
}

/// What uses `dep` once installed reads it so: a hypervisor its provider's detection, Docker its
/// engine, started (the only engine a test starts: this one installed it).
async fn usable(cx: &Context, dep: Dependency) {
    if let Some(provider) = dep.hypervisor() {
        let status = providers::status(cx, provider).await.expect("offered here");
        println!("{status:#?}");
        assert_eq!(status.hypervisor, Some(true), "{} reads it installed", provider.id());
    }
    if dep == Dependency::Docker {
        let engine = if cx.host.os == HostOs::Linux { Engine::Native } else { Engine::DockerDesktop };
        docker::start_engine(cx, engine, elevation(cx).await, &mut |line| println!("{line}"))
            .await
            .expect("the engine installed starts");
        docker::daemon_os(cx).await.expect("its daemon answers");
    }
}

/// Installs `dep` for real when it is missing here and asked for; skips, saying why, else.
async fn installs(dep: Dependency) {
    let cx = here().await;
    if let Some(why) = not_tested(&cx, dep).await {
        skip!("{why}")
    }
    install(&cx, dep).await;
    usable(&cx, dep).await;
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
