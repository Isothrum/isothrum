//! The hypervisor self-test: a throwaway VM, run through a runner of its own. It checks Vagrant
//! answers, the hypervisor the caller picks answers, a small test box is here (added when it
//! isn't), the VM boots, a command runs inside it, and this computer reaches it on a private
//! network. The VM is always taken down, and so are the host networks the test made. A VM an
//! earlier test left that Vagrant no longer knows is removed first, but only when it is surely the
//! test's: on Hyper-V every disk of it is under the test folder, on VirtualBox its description
//! names the test folder.
//!
//! On Hyper-V the start and the command inside share one elevation: one administrator prompt for
//! both ([`isothrum_runner::Running::sharing_elevation`]).

mod boxes;
mod leftover;
mod networks;
mod ping;
mod vboxmanage;

use std::path::Path;
use std::time::Duration;

use isothrum_runner::{Arch, BoxFuture, DownloadToAdopt, Provider, RunOptions, Runner};

pub use boxes::{add_box, box_for, box_provider, candidate_boxes};
pub use leftover::hyperv_leftover_script;

use super::{Progress, throwaway};
use crate::Context;

/// The run's name in the test's own runner.
const RUN: &str = "hypervisor-selftest";
/// The test VM's address on its private network (VirtualBox's default host-only range): the
/// first of these nothing answers on yet. One that answers already (another VM on it) would make
/// the ping pass whatever the test VM does.
pub const ADDRESSES: [&str; 4] = ["192.168.56.250", "192.168.56.249", "192.168.56.248", "192.168.56.247"];
/// Pings, [`PING_GAP`] apart, for the VM to answer on its private network.
const PING_TRIES: u32 = 5;
const PING_GAP: Duration = Duration::from_secs(1);

/// The test VM, as the caller names it.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TestVm<'a> {
    /// How it shows in the hypervisor's own app; also how a VM an earlier test left is found.
    pub name: &'a str,
    /// Its host name inside; also the prefix of its libvirt domain, and the tag of the VirtualBox
    /// description that marks a VM as the test's.
    pub hostname: &'a str,
}

impl<'a> TestVm<'a> {
    /// The test VM named `name` in the hypervisor's app, `hostname` inside.
    #[must_use]
    pub fn new(name: &'a str, hostname: &'a str) -> TestVm<'a> {
        TestVm { name, hostname }
    }
}

/// A step of the hypervisor self-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
    /// A VM an earlier test left was removed (said only when one was, or couldn't be).
    Leftover,
    /// Vagrant answers.
    Vagrant,
    /// The caller picked a hypervisor.
    Provider,
    /// The hypervisor answers.
    Hypervisor,
    /// The test box is here, added when it wasn't.
    Box,
    /// The VM boots.
    Boot,
    /// A command runs inside it.
    Exec,
    /// This computer reaches it on its private network.
    Network,
    /// The host networks the test made were removed (said only when one was, or couldn't be).
    Networks,
}

/// What a step that passed (or was skipped) found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
    /// Vagrant's version line.
    Version(String),
    /// The hypervisor picked.
    Provider(Provider),
    /// It answered.
    Responds,
    /// It has no probe to answer.
    NothingToProbe,
    /// The test box: whether it was here already.
    Box {
        /// Its name.
        name: &'static str,
        /// Here before the test.
        held: bool,
    },
    /// The VM booted from `name` on `provider`.
    Booted {
        /// The box's name.
        name: &'static str,
        /// The hypervisor.
        provider: Provider,
    },
    /// What `uname -srm` printed inside.
    Uname(String),
    /// The VM answered a ping at `address`.
    Reached {
        /// Its address.
        address: &'static str,
    },
    /// Skipped: the hypervisor's plugin gives a VM no private network.
    NoPrivateNetwork(Provider),
    /// Skipped: every address of [`ADDRESSES`] answers already.
    AddressesTaken,
    /// The hypervisors a VM an earlier test left was removed from.
    Removed(Vec<&'static str>),
    /// The host networks removed.
    NetworksRemoved(Vec<String>),
    /// A line a step's tool printed while it ran.
    Line(String),
}

/// Why a step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// What the tool said.
    Said(String),
    /// The runner's refusal or failure, as it reports it.
    Runner(Box<isothrum_runner::ErrorReport>),
    /// A VM of the test's name that isn't the test's is on `hypervisor`: left alone.
    Foreign {
        /// The hypervisor.
        hypervisor: &'static str,
    },
    /// No box exists for `hypervisor` on this computer's CPU (`arch`): the test has nothing to
    /// boot. Hyper-V on Arm, until a box of ours is published.
    NoTestBox {
        /// The hypervisor.
        hypervisor: &'static str,
        /// The computer's CPU.
        arch: Arch,
    },
    /// The VM never answered a ping at `address`; the last answer.
    NoReply {
        /// Its address.
        address: &'static str,
        /// What the last ping said.
        last: String,
    },
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Said(said) => f.write_str(said),
            Failure::Runner(e) => e.fmt(f),
            Failure::Foreign { hypervisor } => write!(f, "{hypervisor} has a VM of the test's name that isn't the test's"),
            Failure::NoTestBox { hypervisor, arch } => write!(
                f,
                "no test VM exists yet for {hypervisor} on {} computers, so the test has nothing to boot here",
                boxes::arch_name(*arch)
            ),
            Failure::NoReply { address, last } => write!(f, "{address} did not answer ping: {last}"),
        }
    }
}

/// A step's progress, as the test says it. [`Progress::Passed`] with [`Found::Line`] is a line
/// its tool printed while it still runs.
pub type StepProgress = Progress<Step, Found, Failure>;

/// How the test went.
#[derive(Debug)]
#[non_exhaustive]
pub struct Ran {
    /// Whether every step passed; why not.
    pub passed: Result<(), Failure>,
    /// Whether the test VM was taken down; why not. None: no VM was started.
    pub cleaned_up: Option<Result<(), Failure>>,
}

/// What the caller decides during the test: the hypervisor, once Vagrant answers; the test box,
/// once the hypervisor answers (added by [`add_box`], or waited for while a fetch ahead of the
/// test adds it).
pub trait Choices: Send + Sync {
    /// The hypervisor to test; why none can be.
    fn provider(&self) -> BoxFuture<'_, Result<Provider, String>>;

    /// Makes sure box `name` is here for `provider`, each line its fetch prints said to `line`:
    /// whether it was here already.
    fn fetch_box<'a>(&'a self, provider: Provider, name: &'static str, line: &'a (dyn Fn(String) + Send + Sync)) -> BoxFuture<'a, Result<bool, String>>;
}

/// Providers whose Vagrant plugin can put the VM on a static private network.
#[must_use]
pub fn has_private_network(p: Provider) -> bool {
    matches!(p, Provider::Virtualbox | Provider::VmwareDesktop | Provider::Parallels | Provider::Libvirt)
}

/// The Hyper-V switch the test VM joins: Windows' own NAT switch (Windows 10 and 11).
pub const HYPERV_SWITCH: &str = "Default Switch";

/// The test's Vagrantfile: `vm` booted from `name` on `p`, at `address` on a private network when
/// it gets one. The VM boots as any deployment's VMs do: on QEMU with the hypervisor's
/// acceleration (vagrant-qemu's default), never software emulation, which boots where no
/// deployment could start. `dir`: the test's folder, which a VirtualBox VM's description names, so
/// a VM an earlier test left is known as the test's.
#[must_use]
pub fn vagrantfile(vm: &TestVm<'_>, p: Provider, name: &str, address: Option<&str>, dir: &Path) -> String {
    let mut v = vec![
        "Vagrant.configure(\"2\") do |config|".to_owned(),
        format!("  config.vm.box = \"{name}\""),
        "  config.vm.boot_timeout = 600".to_owned(),
        "  config.vm.synced_folder \".\", \"/vagrant\", disabled: true".to_owned(),
    ];
    // Vagrant sets a hostname by renewing the guest's DHCP lease (`ifdown -a; ifup -a` on Alpine),
    // and on Hyper-V's Default Switch the address it then gets isn't the one its SSH session is
    // on: `up` failed at "Setting hostname..." with "The SSH connection was unexpectedly closed".
    // On Hyper-V the VM keeps its box's hostname.
    if p != Provider::Hyperv {
        v.push(format!("  config.vm.hostname = \"{}\"", vm.hostname));
    }
    if let Some(address) = address.filter(|_| has_private_network(p)) {
        v.push(format!("  config.vm.network \"private_network\", ip: \"{address}\""));
    }
    // Hyper-V connects a VM to one virtual switch, and Vagrant asks which on its standard input
    // when there are several (WSL 2 adds its own beside the Default Switch): a run has no one to
    // answer, so `vagrant up` waited for ever. The test names Windows' Default Switch.
    if p == Provider::Hyperv {
        v.push(format!("  config.vm.network \"public_network\", bridge: \"{HYPERV_SWITCH}\""));
    }
    let named = vm.name;
    let settings = match p {
        Provider::Virtualbox => format!(
            "    h.name = \"{named}\"\n    h.memory = 512\n    h.customize [\"modifyvm\", :id, \"--description\", {}]\n",
            ruby_quote(&leftover::vbox_description(vm, dir))
        ),
        Provider::Parallels => format!("    h.name = \"{named}\"\n    h.memory = 512\n"),
        Provider::VmwareDesktop => format!("    h.vmx[\"displayName\"] = \"{named}\"\n    h.memory = 512\n"),
        Provider::Libvirt => format!("    h.default_prefix = \"{}-\"\n    h.memory = 512\n", vm.hostname),
        Provider::Hyperv => format!("    h.vmname = \"{named}\"\n"),
        Provider::Utm => format!("    h.name = \"{named}\"\n"),
        // vagrant-qemu's default is 4G, much more than a test VM needs.
        Provider::Qemu => "    h.memory = \"1G\"\n".to_owned(),
        Provider::VmwareEsxi => String::new(),
    };
    if !settings.is_empty() {
        v.push(format!("  config.vm.provider \"{}\" do |h|\n{settings}  end", p.id()));
    }
    v.push("end\n".to_owned());
    v.join("\n")
}

/// `s` as a Ruby single-quoted string (a Windows path's backslashes kept as they are).
fn ruby_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A read-only command that succeeds only when `p`'s service is up and usable by this account,
/// and opens no window.
fn responds(p: Provider) -> Option<(String, &'static [&'static str])> {
    let tool = |t: &str| Some(t.to_owned());
    let (program, args): (Option<String>, &'static [&'static str]) = match p {
        Provider::Virtualbox => (tool("VBoxManage"), &["list", "hostinfo"]),
        Provider::VmwareDesktop => (tool("vmrun"), &["list"]),
        Provider::Parallels => (tool("prlctl"), &["list", "--all"]),
        Provider::Libvirt => (tool("virsh"), &["-c", "qemu:///system", "list", "--all"]),
        // `utmctl list` opens UTM with its window in front: it is asked as a start asks it
        // ([`crate::providers::ready::utm`]), hidden.
        Provider::Utm | Provider::Hyperv | Provider::Qemu | Provider::VmwareEsxi => (None, &[]),
    };
    program.map(|found| (found, args))
}

/// Whether `program` answers `args`: from `PATH`, else where its app installs it (VMware's
/// `vmrun`, Parallels' `prlctl`), as detection finds it. What it said when neither answers.
async fn answers(cx: &Context, program: &str, args: &[&str]) -> Result<(), String> {
    let on_path = cx.read_ok(program, args).await;
    let Err(said) = on_path else { return Ok(()) };
    match cx.host.devices.app_tool(program).into_iter().find(|t| t.is_file()) {
        Some(at) => cx.read_ok(&at.display().to_string(), args).await.map(drop),
        None => Err(said),
    }
}

/// One ping with a 3 s wait, in this OS's flags.
fn ping_args(cx: &Context, address: &str) -> Vec<String> {
    let flags: &[&str] = match cx.host.os {
        isothrum_runner::HostOs::Windows => &["-n", "1", "-w", "3000"],
        isothrum_runner::HostOs::Macos => &["-c", "1", "-t", "3"],
        isothrum_runner::HostOs::Linux => &["-c", "1", "-W", "3"],
    };
    flags.iter().copied().chain([address]).map(str::to_owned).collect()
}

/// What one ping of `address` printed; `Err` when `ping` couldn't run.
async fn ping_once(cx: &Context, address: &str) -> Result<String, String> {
    let spec = isothrum_runner::ProcessSpec::new("ping", ping_args(cx, address));
    let captured = cx.probe_within(cx.probe_timeout, &spec).await.map_err(|e| e.to_string())?;
    Ok(crate::exec::all_lines(&captured))
}

/// Whether `address` answers one ping.
async fn answers_ping(cx: &Context, address: &str) -> bool {
    ping_once(cx, address).await.is_ok_and(|out| ping::replied(&out))
}

/// The first test address nothing answers on yet.
async fn free_address(cx: &Context) -> Option<&'static str> {
    for address in ADDRESSES {
        if !answers_ping(cx, address).await {
            return Some(address);
        }
    }
    None
}

/// Runs the test, its runner's home and project in `dir` (the test's own folder), `choices`
/// deciding the hypervisor and fetching the box, each step's progress said to `progress`.
pub async fn run(cx: &Context, dir: &Path, vm: &TestVm<'_>, choices: &dyn Choices, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Ran {
    let (runner, project) = match throwaway(cx, dir, "Vagrantfile", "") {
        Ok(test) => test,
        Err(e) => {
            return Ran {
                passed: Err(Failure::Said(e)),
                cleaned_up: None,
            };
        }
    };
    match clear_earlier(cx, &runner, &project, dir, vm).await {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => progress(Progress::Passed(Step::Leftover, Found::Removed(removed))),
        Err(why) => {
            progress(Progress::Failed(Step::Leftover, why.clone()));
            return Ran {
                passed: Err(why),
                cleaned_up: None,
            };
        }
    }
    let mut test = Test {
        cx,
        runner: &runner,
        project: &project,
        dir,
        vm,
        provider: None,
        before: networks::Networks::default(),
        run: None,
    };
    let passed = Box::pin(test.steps(choices, progress)).await;
    let cleaned_up = match test.provider {
        Some(provider) => {
            // Through the handle that started it, when there is one: on Hyper-V, its elevation.
            let down = match &test.run {
                Some(run) => down_with(run, provider).await,
                None => down(&runner, &project, provider).await,
            };
            if down.is_ok() {
                remove_networks(cx, provider, &test.before, progress).await;
            }
            Some(down)
        }
        _ => None,
    };
    Ran { passed, cleaned_up }
}

/// The test's run, opened.
fn open<'a>(runner: &'a Runner, project: &Path) -> Result<isothrum_runner::Running<'a>, Failure> {
    super::open(runner, project, RUN).map_err(Failure::Said)
}

/// Takes the test VM down on `provider`.
async fn down(runner: &Runner, project: &Path, provider: Provider) -> Result<(), Failure> {
    down_with(&open(runner, project)?, provider).await
}

/// Takes the test VM down on `provider` through `run`.
async fn down_with(run: &isothrum_runner::Running<'_>, provider: Provider) -> Result<(), Failure> {
    let opts = RunOptions::new().with_provider(provider);
    run.down(&opts, &super::quiet).await.map_err(|e| Failure::Runner(Box::new(e.report())))
}

/// What a test interrupted last time may have left: its VM, taken down through the runner when
/// it still records it as up, else found by its name and removed when it is surely the test's
/// ([`leftover`]): the hypervisors it was removed from.
async fn clear_earlier(cx: &Context, runner: &Runner, project: &Path, dir: &Path, vm: &TestVm<'_>) -> Result<Vec<&'static str>, Failure> {
    let recorded = isothrum_runner::RunName::new(RUN).ok().and_then(|name| runner.run_record(&name).ok().flatten());
    // Best effort: what the runner couldn't take down is found by its name next.
    if let Some(record) = recorded.filter(|r| r.state != isothrum_runner::RunState::Absent)
        && let Some(provider) = record.provider
    {
        drop(down(runner, project, provider).await);
    }
    leftover::clear(cx, dir, vm).await
}

/// Removes the host networks the test made, said when it removed one or couldn't.
async fn remove_networks(cx: &Context, provider: Provider, before: &networks::Networks, progress: &(dyn Fn(StepProgress) + Send + Sync)) {
    match networks::remove_made_since(cx, provider, before).await {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => progress(Progress::Passed(Step::Networks, Found::NetworksRemoved(removed))),
        Err(why) => progress(Progress::Failed(Step::Networks, Failure::Said(why))),
    }
}

/// What the steps work on.
struct Test<'a> {
    cx: &'a Context,
    runner: &'a Runner,
    project: &'a Path,
    dir: &'a Path,
    vm: &'a TestVm<'a>,
    /// The hypervisor, once picked.
    provider: Option<Provider>,
    /// The host networks there before the VM booted.
    before: networks::Networks,
    /// The test's run, once its VM is started: its operations share one elevation.
    run: Option<isothrum_runner::Running<'a>>,
}

impl<'a> Test<'a> {
    /// The steps, in order: the first that fails ends the test.
    async fn steps(&mut self, choices: &dyn Choices, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Result<(), Failure> {
        let cx = self.cx;
        let step = super::Steps(progress);
        step.run(Step::Vagrant, async {
            let v = cx.read_ok("vagrant", &["--version"]).await.map_err(Failure::Said)?;
            Ok(Found::Version(v.trim().to_owned()))
        })
        .await?;
        let mut picked = None;
        step.run(Step::Provider, async {
            let p = choices.provider().await.map_err(Failure::Said)?;
            picked = Some(p);
            Ok(Found::Provider(p))
        })
        .await?;
        let Some(provider) = picked else { return Ok(()) };
        step.run(Step::Hypervisor, async {
            if provider == Provider::Utm {
                let ready = crate::providers::ready::utm(cx, crate::providers::ready::UTM_WAIT, &mut || {}).await;
                return ready.map(|()| Found::Responds).map_err(|e| Failure::Said(e.to_string()));
            }
            let Some((program, args)) = responds(provider) else {
                return Ok(Found::NothingToProbe);
            };
            answers(cx, &program, args).await.map(|()| Found::Responds).map_err(Failure::Said)
        })
        .await?;
        self.before = networks::Networks::here(cx, provider).await;
        let chosen = box_for(cx, provider).await;
        let box_line = |l: String| progress(Progress::Passed(Step::Box, Found::Line(l)));
        let mut name = "";
        step.run(Step::Box, async {
            let (chosen, _) = chosen.ok_or(Failure::NoTestBox {
                hypervisor: provider.id(),
                arch: cx.host.arch,
            })?;
            let held = choices.fetch_box(provider, chosen, &box_line).await.map_err(Failure::Said)?;
            name = chosen;
            Ok(Found::Box { name: chosen, held })
        })
        .await?;
        // An address for the ping that nothing answers on before the VM exists.
        let address = if has_private_network(provider) { free_address(cx).await } else { None };
        let written = std::fs::write(self.project.join("Vagrantfile"), vagrantfile(self.vm, provider, name, address, self.dir));
        written.map_err(|e| Failure::Said(format!("{} couldn't be written: {e}", self.project.display())))?;
        self.provider = Some(provider);
        self.boot_and_exec(provider, name, progress).await?;
        network(cx, provider, address, progress).await
    }

    /// Boots the VM from box `name` on `provider`, then runs a command inside, through one
    /// handle: on Hyper-V, one elevation for both (and for the VM's removal after).
    async fn boot_and_exec(&mut self, provider: Provider, name: &'static str, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Result<(), Failure> {
        let step = super::Steps(progress);
        let run: &isothrum_runner::Running<'a> = self.run.insert(open(self.runner, self.project)?.sharing_elevation());
        let opts = RunOptions::new().with_provider(provider);
        let boot_line = |e: &isothrum_runner::Event| {
            if let isothrum_runner::EventKind::Log { line, .. } = &e.kind {
                progress(Progress::Passed(Step::Boot, Found::Line(line.clone())));
            }
        };
        let up = Box::pin(async {
            run.up(&opts, &boot_line)
                .await
                .map(|_| Found::Booted { name, provider })
                .map_err(|e| Failure::Runner(Box::new(e.report())))
        });
        step.run(Step::Boot, up).await?;
        step.run(Step::Exec, async {
            let argv = ["uname", "-srm"].map(str::to_owned);
            let ran = run
                .exec("default", &argv, &opts, &super::quiet)
                .await
                .map_err(|e| Failure::Runner(Box::new(e.report())))?;
            Ok(Found::Uname(uname_line(&ran.stdout)))
        })
        .await
    }
}

/// The VM answers ping on its private network (skipped without one, or a free address).
async fn network(cx: &Context, provider: Provider, address: Option<&'static str>, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Result<(), Failure> {
    let Some(address) = address else {
        let why = if has_private_network(provider) {
            Found::AddressesTaken
        } else {
            Found::NoPrivateNetwork(provider)
        };
        progress(Progress::Skipped(Step::Network, why));
        return Ok(());
    };
    super::Steps(progress)
        .run(Step::Network, async {
            let mut last = String::new();
            for _ in 0..PING_TRIES {
                match ping_once(cx, address).await {
                    Ok(out) if ping::replied(&out) => return Ok(Found::Reached { address }),
                    Ok(out) => out
                        .lines()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .nth(1)
                        .unwrap_or("no reply")
                        .clone_into(&mut last),
                    Err(e) => last = e,
                }
                tokio::time::sleep(PING_GAP).await;
            }
            Err(Failure::NoReply { address, last })
        })
        .await
}

/// What `uname -srm` printed: its first line that says something.
fn uname_line(out: &str) -> String {
    out.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned()
}

/// The box `name` added for `provider` by this process: for a caller that keeps a record of its
/// downloads.
fn adopted(name: &str, provider: Provider, version: String) -> DownloadToAdopt {
    DownloadToAdopt::Box {
        name: name.to_owned(),
        provider: box_provider(provider).to_owned(),
        version,
    }
}

#[cfg(test)]
mod tests;
