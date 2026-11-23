//! What a run needs on this computer ([`requirements::needs`]), against the tools as they are
//! installed here: a tool that answers reads as present, with no plan; one that doesn't reads as
//! missing, with the plan that brings it. Asking installs nothing: no program it runs installs,
//! elevates or starts anything.

use std::sync::{Arc, Mutex, PoisonError};

use isothrum_machine::install::Dependency;
use isothrum_machine::providers::{self, Provider};
use isothrum_machine::requirements::{self, Asking, Need, Requirement, Target, Uses};
use isothrum_machine::tools::ovftool;
use isothrum_machine::{Context, HostOs};
use isothrum_runner::{BoxFuture, Cloud, Exit, Format, OnLine, ProcessError, ProcessRunner, ProcessSpec, SystemProcesses};

use crate::answer;

/// Every hypervisor a run can name.
const PROVIDERS: [Provider; 8] = [
    Provider::Virtualbox,
    Provider::VmwareDesktop,
    Provider::Hyperv,
    Provider::Parallels,
    Provider::Libvirt,
    Provider::Qemu,
    Provider::Utm,
    Provider::VmwareEsxi,
];

/// The real tools, each command line they are asked kept.
#[derive(Default)]
struct Recorded {
    tools: SystemProcesses,
    lines: Mutex<Vec<String>>,
}

impl Recorded {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl ProcessRunner for Recorded {
    fn run<'a>(&'a self, spec: &'a ProcessSpec, on_line: OnLine<'a>) -> BoxFuture<'a, Result<Exit, ProcessError>> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).push(spec.line());
        self.tools.run(spec, on_line)
    }
}

/// Every run this computer could be asked for: Compose, Vagrant on each hypervisor that applies
/// here, Terraform on each target.
fn every_use(cx: &Context) -> Vec<Uses> {
    let mut all = vec![Uses::new(Format::Compose), Uses::new(Format::Vagrant), Uses::new(Format::Terraform)];
    all.extend(PROVIDERS.into_iter().filter(|p| providers::applicable(*p, &cx.host)).map(Uses::vagrant));
    all.extend([Target::Proxmox, Target::Esxi].map(Uses::terraform));
    for cloud in [Cloud::Aws, Cloud::Azure, Cloud::Gcp, Cloud::Digitalocean] {
        all.push(Uses::terraform(Target::Cloud { cloud }));
    }
    all
}

/// Whether `need`'s requirement is met here, asked of the machine another way than the crate
/// does; none when no other way tells.
async fn met_here(cx: &Context, need: &Need, path: Option<&std::ffi::OsStr>) -> Option<bool> {
    match need.requirement {
        Requirement::Tool { dependency } if dependency.hypervisor().is_none() => {
            let row = dependency.row();
            let (program, args) = row.probe?;
            let app = row
                .mac_app
                .filter(|_| cx.host.os == HostOs::Macos)
                .is_some_and(|a| std::path::Path::new(a).exists());
            Some(app || answer(cx, program, args).await.is_some())
        }
        Requirement::ComposePlugin => Some(answer(cx, "docker", &["compose", "version"]).await.is_some()),
        Requirement::EngineRunning => Some(
            answer(cx, "docker", &["info", "--format", "{{.ServerVersion}}"])
                .await
                .is_some_and(|v| !v.trim().is_empty()),
        ),
        Requirement::Ovftool => Some(ovftool::locate(cx, None, path).is_some()),
        _ => None,
    }
}

/// Each run's needs read each tool as it is here: present with no plan, or missing with one.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn each_runs_needs_read_the_tools_as_they_are_here() {
    let cx = crate::here().await;
    let path = std::env::var_os("PATH");
    let asking = Asking {
        path: path.as_deref(),
        ..Asking::default()
    };
    let (mut present, mut missing) = (Vec::new(), Vec::new());
    for uses in every_use(&cx) {
        for need in requirements::needs(&cx, &uses.requirements(), &asking).await {
            match need.present {
                Some(true) => assert!(need.plan.is_none(), "{uses:?}: a requirement met here has no plan: {need:?}"),
                _ => assert!(need.plan.is_some(), "{uses:?}: a requirement not known met has a plan: {need:?}"),
            }
            if let Some(met) = met_here(&cx, &need, path.as_deref()).await {
                assert_eq!(need.present, Some(met), "{uses:?}: {need:?}");
            }
            let name = format!("{:?}", need.requirement);
            if need.missing() { missing.push(name) } else { present.push(name) }
        }
    }
    present.sort();
    present.dedup();
    missing.sort();
    missing.dedup();
    println!("present here: {}", present.join(", "));
    println!("missing here: {}", missing.join(", "));
}

/// Asking every run's needs runs no install, no elevation and no start: only reads.
#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn asking_what_runs_need_installs_nothing() {
    let recorded = Arc::new(Recorded::default());
    let runner: Arc<dyn ProcessRunner> = Arc::<Recorded>::clone(&recorded);
    let cx = Context::here(runner).await;
    let before: Vec<Option<bool>> = tools_present(&cx).await;
    for uses in every_use(&cx) {
        requirements::needs(&cx, &uses.requirements(), &Asking::default()).await;
    }
    let ran = recorded.lines();
    let changing: Vec<&String> = ran.iter().filter(|l| changes_the_machine(l)).collect();
    assert!(changing.is_empty(), "asking ran what changes the machine: {changing:#?}");
    assert_eq!(tools_present(&cx).await, before, "every tool is as it was");
}

/// Whether a recorded command line can change the machine: an installer, an elevation, a
/// package manager, an install or a start. A program asked only its version (`apt-get
/// --version`, how [`Context::here`] finds the package manager) reads.
fn changes_the_machine(line: &str) -> bool {
    let words: Vec<&str> = line.trim_start_matches("$ ").split_whitespace().collect();
    if let [_, "--version"] = words.as_slice() {
        return false;
    }
    let program = words.first().copied().unwrap_or_default();
    ["pkexec", "sudo", "winget", "msiexec", "dism", "apt-get", "dnf", "open", "softwareupdate"].contains(&program)
        || words.windows(2).any(|w| matches!(w, ["plugin" | "--cask", "install"] | [_, "start"]))
        || words.contains(&"install")
}

#[test]
fn a_version_probe_reads_and_an_install_changes_the_machine() {
    for read in ["apt-get --version", "$ dnf --version", "vagrant plugin list", "VBoxManage --version"] {
        assert!(!changes_the_machine(read), "{read}");
    }
    for change in [
        "apt-get install -y qemu-system",
        "$ dnf --version install x",
        "pkexec apt-get --version",
        "vagrant plugin install vagrant-libvirt",
        "systemctl start libvirtd",
        "brew install --cask utm",
    ] {
        assert!(changes_the_machine(change), "{change}");
    }
}

/// Whether each tool a setup installs is here, in [`Dependency::ALL`]'s order.
async fn tools_present(cx: &Context) -> Vec<Option<bool>> {
    let mut found = Vec::new();
    for dep in Dependency::ALL {
        found.push(isothrum_machine::install::present(cx, dep).await);
    }
    found
}
