//! The server self-test: a throwaway VM on a server, run through a runner of its own. It checks
//! the server answers, writes the test's own project (a Terraform module on Proxmox VE, a
//! Vagrantfile for Vagrant's `vmware_esxi` on ESXi), takes down what an earlier test left, brings
//! the VM up step by step as the tool's lines end each, waits for it to accept SSH, runs a command
//! inside, and always takes it down.
//!
//! The runner's home and the project are in the test's folder, which the caller keeps from one
//! test to the next: the runner's record of the run is how a VM an interrupted test left is found
//! and removed first, and its Terraform plugin cache is reused.
//!
//! - [`Proxmox`]: the VM on the server's bridge (DHCP), a second card on a network of the test's
//!   own, the guest agent reporting its address; signed in to over SSH with the caller's key or
//!   the test runner's own, through the caller's jump host when only it reaches the bridge.
//! - [`Esxi`]: the VM uploaded from a Vagrant box by the `vagrant-vmware-esxi` plugin and the OVF
//!   Tool, on the host's port group.

mod course;
mod esxi;
mod proxmox;

use std::path::Path;
use std::time::Duration;

use isothrum_runner::{ErrorReport, RunName, RunOptions, RunState, Runner};

#[cfg(test)]
use course::Deployment;
use course::{Course, OnRunner, test};
#[cfg(test)]
use isothrum_runner::BoxFuture;

pub use esxi::Esxi;
pub use proxmox::{Key, Proxmox, SignIn};

use super::hypervisor::TestVm;
use super::{Progress, throwaway};
use crate::Context;

/// The run's name in the test's own runner.
const RUN: &str = "server-selftest";
/// How long the server has to answer on its port.
const HOST_WAIT: Duration = Duration::from_secs(30);
/// How long the VM has, once up, to accept SSH.
const SSH_WAIT: Duration = Duration::from_secs(300);
/// The gap between two tries of a port or of SSH.
const RETRY: Duration = Duration::from_secs(3);
/// How long one try to open a port may take.
const CONNECT_LIMIT: Duration = Duration::from_secs(4);

/// The server the test runs on.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Server {
    /// A Proxmox VE server, through its API (Terraform's `bpg/proxmox`); boxed, being by far the
    /// larger.
    Proxmox(Box<Proxmox>),
    /// An ESXi host, from this computer (Vagrant's `vmware_esxi`).
    Esxi(Esxi),
}

impl Server {
    /// Where the server answers (`host:port`): its API's port on Proxmox VE, its SSH port on ESXi.
    #[must_use]
    pub fn address(&self) -> String {
        match self {
            Server::Proxmox(p) => p.api_address(),
            Server::Esxi(e) => e.ssh_address(),
        }
    }
}

/// A step of the server self-test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Step {
    /// The server answers on its port.
    Host,
    /// The test's project is written, and what an earlier test left is taken down.
    Prepare,
    /// The VM's image is on the server (Proxmox VE: the cloud image downloaded there; ESXi: the
    /// box uploaded to the datastore).
    Image,
    /// The test's own network is made (Proxmox VE only).
    Network,
    /// The VM is created and started.
    Vm,
    /// The VM has an address on the server's network.
    Address,
    /// The VM accepts SSH.
    Ssh,
    /// A command runs inside it.
    Exec,
    /// Everything the test made is taken down.
    Down,
}

/// What a step that passed found, or a line it printed while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Found {
    /// Nothing to say.
    Nothing,
    /// The server answered at this address (`host:port`).
    Answers(String),
    /// What an earlier test left is being taken down (said while [`Step::Prepare`] runs).
    Removing,
    /// The box the VM is built from, as Vagrant named it, and the architecture of its build.
    Box {
        /// Its name.
        name: String,
        /// Its version, when said.
        version: Option<String>,
        /// The architecture of its build, when known (several, `or`-joined, when the box store
        /// holds several and none was named).
        arch: Option<String>,
    },
    /// A line the step's tool printed while it runs, with how many bytes the step moves when
    /// known (the box the OVF Tool uploads).
    Line {
        /// The line.
        text: String,
        /// The step's size.
        bytes: Option<u64>,
    },
    /// What `uname -sm` printed inside.
    Uname(String),
}

/// Why a step failed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// The server gave no answer at this address (`host:port`).
    Unreachable(String),
    /// What went wrong outside the runner (the project couldn't be written).
    Said(String),
    /// What an earlier test left couldn't be taken down: the runner's report.
    Leftover(Box<ErrorReport>),
    /// The server couldn't download the VM's image (Proxmox VE).
    ImageDownload(Box<ErrorReport>),
    /// A bridge the VM needs isn't on the server's node (Proxmox VE: the test's own network,
    /// when the node doesn't load its SDN configuration).
    NoBridge(Box<ErrorReport>),
    /// A VM of the test's name is on the host already, which the test didn't make (ESXi).
    NameTaken,
    /// The OVF Tool couldn't upload the VM (ESXi): its last error line, else the runner's message.
    Upload(String),
    /// Vagrant couldn't get the box for the server's hypervisor (ESXi).
    NoBox(Box<ErrorReport>),
    /// The server couldn't power the VM on (ESXi).
    PowerOn(Box<ErrorReport>),
    /// The VM got no address on the server's network in time.
    NoAddress(Box<ErrorReport>),
    /// The VM didn't accept SSH in time: the last try's report.
    NoSsh(Box<ErrorReport>),
    /// The command inside ran and failed: its exit status and what it printed.
    Command {
        /// Its exit status.
        exit: i32,
        /// Its output.
        output: String,
    },
    /// The runner's refusal or failure, as it reports it.
    Runner(Box<ErrorReport>),
}

impl Failure {
    /// The runner's report it holds, when it holds one.
    #[must_use]
    pub fn report(&self) -> Option<&ErrorReport> {
        match self {
            Failure::Leftover(r)
            | Failure::ImageDownload(r)
            | Failure::NoBridge(r)
            | Failure::NoBox(r)
            | Failure::PowerOn(r)
            | Failure::NoAddress(r)
            | Failure::NoSsh(r)
            | Failure::Runner(r) => Some(r),
            Failure::Unreachable(_) | Failure::Said(_) | Failure::NameTaken | Failure::Upload(_) | Failure::Command { .. } => None,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Unreachable(address) => write!(f, "no answer on {address}"),
            Failure::Said(said) | Failure::Upload(said) => f.write_str(said),
            Failure::NameTaken => f.write_str("a VM of the test's name is on the host already"),
            Failure::Command { exit, output } => write!(f, "the command inside exited with {exit}: {}", output.trim()),
            other => other.report().map_or(Ok(()), |r| r.fmt(f)),
        }
    }
}

/// A step's progress, as the test says it. [`Progress::Passed`] with [`Found::Line`] or
/// [`Found::Removing`] is said while the step still runs.
pub type StepProgress = Progress<Step, Found, Failure>;

/// How the test went.
#[derive(Debug)]
#[non_exhaustive]
pub struct Ran {
    /// Whether every step passed; why not.
    pub passed: Result<(), Failure>,
    /// Whether what the test made was taken down; why not. None: it made nothing (the server
    /// didn't answer, the project couldn't be written, an earlier test's VM couldn't be removed,
    /// or `up` was refused before it acted, a tool it needs missing).
    pub cleaned_up: Option<Result<(), Failure>>,
}

/// Runs the test on `server`, its runner's home and project in `dir` (the test's own folder,
/// kept from one test to the next), its VM named `vm`, each step's progress said to `progress`.
pub async fn run(cx: &Context, dir: &Path, server: &Server, vm: &TestVm<'_>, progress: &(dyn Fn(StepProgress) + Send + Sync)) -> Ran {
    let step = super::Steps(progress);
    let address = server.address();
    let reached = step.run(Step::Host, async {
        if answers(&address, HOST_WAIT).await {
            Ok(Found::Answers(address.clone()))
        } else {
            Err(Failure::Unreachable(address.clone()))
        }
    });
    if let Err(why) = reached.await {
        return Ran {
            passed: Err(why),
            cleaned_up: None,
        };
    }
    match server {
        Server::Proxmox(p) => {
            let (file, text) = proxmox::project();
            Box::pin(on_runner(cx, dir, (file, &text), progress, async |runner: &Runner| {
                proxmox::prepared(p, vm, runner).await
            }))
            .await
        }
        Server::Esxi(e) => {
            let (file, text) = e.project(vm);
            Box::pin(on_runner(cx, dir, (file, &text), progress, async |_: &Runner| {
                Ok((e.options(), esxi::course(e)))
            }))
            .await
        }
    }
}

/// The test through a runner of its own in `dir`, its project `file` holding `text`; `ready`
/// gives the run's options and how it comes up, once the runner is there.
async fn on_runner<C: Course>(
    cx: &Context,
    dir: &Path,
    (file, text): (&str, &str),
    progress: &(dyn Fn(StepProgress) + Send + Sync),
    ready: impl AsyncFnOnce(&Runner) -> Result<(RunOptions, C), String>,
) -> Ran {
    progress(Progress::Running(Step::Prepare));
    let opened = async {
        let (runner, project) = throwaway(cx, dir, file, text)?;
        let (opts, course) = ready(&runner).await?;
        Ok::<_, String>((runner, project, opts, course))
    };
    // Nothing is made yet: the preparation's failure ends the test.
    let unprepared = |why: String| {
        let why = Failure::Said(why);
        progress(Progress::Failed(Step::Prepare, why.clone()));
        Ran {
            passed: Err(why),
            cleaned_up: None,
        }
    };
    let (runner, project, opts, course) = match opened.await {
        Ok(opened) => opened,
        Err(why) => return unprepared(why),
    };
    let run = match super::open(&runner, &project, RUN) {
        Ok(run) => run,
        Err(why) => return unprepared(why),
    };
    let left = RunName::new(RUN)
        .ok()
        .and_then(|name| runner.run_record(&name).ok().flatten())
        .is_some_and(|r| r.state != RunState::Absent);
    let deployment = OnRunner {
        run,
        opts,
        machine: course.machine(),
    };
    Box::pin(test(&deployment, left, course, progress, SSH_WAIT)).await
}

/// Whether `address` (`host:port`) accepts a TCP connection within `within`, tried every
/// [`RETRY`].
async fn answers(address: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if matches!(tokio::time::timeout(CONNECT_LIMIT, tokio::net::TcpStream::connect(address)).await, Ok(Ok(_))) {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(RETRY).await;
    }
}

#[cfg(test)]
mod tests;
