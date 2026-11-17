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

mod esxi;
mod proxmox;

use std::path::Path;
use std::time::Duration;

use isothrum_runner::{BoxFuture, ErrorReport, RunName, RunOptions, RunState, Runner};

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
    /// A Proxmox VE server, through its API (Terraform's `bpg/proxmox`).
    Proxmox(Proxmox),
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
    /// didn't answer, the project couldn't be written, or an earlier test's VM couldn't be
    /// removed).
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

/// What the test drives: the run of its project (the runner's; a scripted one in tests).
trait Deployment: Sync {
    /// Brings it up, each line its tool prints said to `line`.
    fn up<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>>;
    /// Runs `argv` inside the VM: its exit status and output.
    fn exec<'a>(&'a self, argv: &'a [&'a str]) -> BoxFuture<'a, Result<(i32, String), Box<ErrorReport>>>;
    /// Takes it down, each line its tool prints said to `line`.
    fn down<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>>;
}

/// The test's run in its runner, with its options and the VM's machine name.
struct OnRunner<'r> {
    run: isothrum_runner::Running<'r>,
    opts: RunOptions,
    machine: &'static str,
}

/// Events said to `line` as the lines their tool printed.
fn lines(line: &(dyn Fn(&str) + Send + Sync)) -> impl Fn(&isothrum_runner::Event) + Send + Sync + '_ {
    move |e: &isothrum_runner::Event| {
        if let isothrum_runner::EventKind::Log { line: said, .. } = &e.kind {
            line(said);
        }
    }
}

impl Deployment for OnRunner<'_> {
    fn up<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        Box::pin(async move { self.run.up(&self.opts, &lines(line)).await.map(drop).map_err(|e| Box::new(e.report())) })
    }

    fn exec<'a>(&'a self, argv: &'a [&'a str]) -> BoxFuture<'a, Result<(i32, String), Box<ErrorReport>>> {
        Box::pin(async move {
            let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
            let ran = self
                .run
                .exec(self.machine, &argv, &self.opts, &super::quiet)
                .await
                .map_err(|e| Box::new(e.report()))?;
            Ok((ran.exit, ran.stdout))
        })
    }

    fn down<'a>(&'a self, line: &'a (dyn Fn(&str) + Send + Sync)) -> BoxFuture<'a, Result<(), Box<ErrorReport>>> {
        Box::pin(async move { self.run.down(&self.opts, &lines(line)).await.map_err(|e| Box::new(e.report())) })
    }
}

/// How one server's test VM comes up, from its tool's lines.
trait Course: Send + Sync {
    /// The steps `up` goes through, in order: each ended by a line ([`Course::ends`]), the last
    /// by `up` succeeding. [`Step::Ssh`] among them when `up` waits for SSH itself.
    const UP: &'static [Step];

    /// The VM's machine name in the run.
    fn machine(&self) -> &'static str;

    /// The step `line` ends, as its index in [`Course::UP`]; what it says may be noted.
    fn ends(&mut self, line: &str) -> Option<usize>;

    /// How many bytes step `at` of [`Course::UP`] moves, once known.
    fn bytes(&self, _at: usize) -> Option<u64> {
        None
    }

    /// What step `at` of [`Course::UP`] found when it ends.
    fn found(&self, _at: usize) -> Found {
        Found::Nothing
    }

    /// A failure of `up`, as the step it belongs to (its index in [`Course::UP`]) and why.
    fn explain(&self, report: &ErrorReport) -> Option<(usize, Failure)>;
}

/// The test once its run is open: what an earlier test left taken down (`left`: the runner
/// records it), then up step by step, SSH waited for (within `ssh_wait`) when `up` doesn't, the
/// command, and always the teardown.
async fn test<C: Course>(run: &impl Deployment, left: bool, course: C, progress: &(dyn Fn(StepProgress) + Send + Sync), ssh_wait: Duration) -> Ran {
    if left {
        progress(Progress::Passed(Step::Prepare, Found::Removing));
        if let Err(report) = run.down(&|_| {}).await {
            let why = Failure::Leftover(report);
            progress(Progress::Failed(Step::Prepare, why.clone()));
            return Ran {
                passed: Err(why),
                cleaned_up: None,
            };
        }
    }
    progress(Progress::Passed(Step::Prepare, Found::Nothing));
    let passed = Box::pin(steps(run, course, progress, ssh_wait)).await;
    progress(Progress::Running(Step::Down));
    let down = run
        .down(&|l| {
            progress(Progress::Passed(
                Step::Down,
                Found::Line {
                    text: l.to_owned(),
                    bytes: None,
                },
            ));
        })
        .await;
    let cleaned_up = match down {
        Ok(()) => {
            progress(Progress::Passed(Step::Down, Found::Nothing));
            Ok(())
        }
        Err(report) => {
            let why = Failure::Runner(report);
            progress(Progress::Failed(Step::Down, why.clone()));
            Err(why)
        }
    };
    Ran {
        passed,
        cleaned_up: Some(cleaned_up),
    }
}

/// Up, SSH, the command: the first that fails ends them.
async fn steps<C: Course>(run: &impl Deployment, course: C, progress: &(dyn Fn(StepProgress) + Send + Sync), ssh_wait: Duration) -> Result<(), Failure> {
    let course = std::sync::Mutex::new(UpSteps::new(course, progress));
    lock(&course).start();
    let up = run.up(&|line| lock(&course).line(line)).await;
    let mut course = course.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
    match up {
        Ok(()) => course.finish(),
        Err(report) => return Err(course.fail(report)),
    }
    let step = super::Steps(progress);
    if !C::UP.contains(&Step::Ssh) {
        step.run(Step::Ssh, wait_ssh(run, ssh_wait)).await?;
    }
    step.run(Step::Exec, async {
        let (exit, output) = run.exec(&["uname", "-sm"]).await.map_err(Failure::Runner)?;
        let said = output.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default().to_owned();
        if exit == 0 && said.contains("Linux") {
            Ok(Found::Uname(said))
        } else {
            Err(Failure::Command { exit, output })
        }
    })
    .await
}

/// `m`, locked whatever a panic left.
fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Tries a command inside the VM until one runs, every [`RETRY`], within `within`: SSH answers.
async fn wait_ssh(run: &impl Deployment, within: Duration) -> Result<Found, Failure> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        match run.exec(&["true"]).await {
            Ok(_) => return Ok(Found::Nothing),
            Err(report) if tokio::time::Instant::now() >= deadline => return Err(Failure::NoSsh(report)),
            Err(_) => tokio::time::sleep(RETRY).await,
        }
    }
}

/// Where `up` is among a course's steps: the first not yet done, moved on by the lines that end
/// each.
struct UpSteps<'p, C> {
    course: C,
    progress: &'p (dyn Fn(StepProgress) + Send + Sync),
    done: usize,
}

impl<'p, C: Course> UpSteps<'p, C> {
    fn new(course: C, progress: &'p (dyn Fn(StepProgress) + Send + Sync)) -> Self {
        UpSteps { course, progress, done: 0 }
    }

    /// Says the running step started.
    fn start(&self) {
        if let Some(step) = C::UP.get(self.done) {
            (self.progress)(Progress::Running(*step));
        }
    }

    /// The running step passed.
    fn pass(&mut self) {
        if let Some(step) = C::UP.get(self.done) {
            (self.progress)(Progress::Passed(*step, self.course.found(self.done)));
        }
        self.done += 1;
    }

    /// A line of `up`: the end of a step (and of any before it), else the running step's latest
    /// line.
    fn line(&mut self, line: &str) {
        match self.course.ends(line) {
            Some(at) if at >= self.done => {
                while self.done <= at.min(C::UP.len().saturating_sub(1)) {
                    self.pass();
                }
                self.start();
            }
            _ => {
                if let Some(step) = C::UP.get(self.done) {
                    let bytes = self.course.bytes(self.done);
                    (self.progress)(Progress::Passed(*step, Found::Line { text: line.to_owned(), bytes }));
                }
            }
        }
    }

    /// `up` succeeded: every step is done.
    fn finish(&mut self) {
        while self.done < C::UP.len() {
            self.pass();
        }
    }

    /// `up` failed with `report`: the step it failed at says why, the ones before it that no
    /// line ended are skipped.
    fn fail(&mut self, report: Box<ErrorReport>) -> Failure {
        let last = C::UP.len().saturating_sub(1);
        let (at, why) = match self.course.explain(&report) {
            Some((at, why)) => (at.max(self.done).min(last), why),
            None => (self.done.min(last), Failure::Runner(report)),
        };
        while self.done < at {
            if let Some(step) = C::UP.get(self.done) {
                (self.progress)(Progress::Skipped(*step, Found::Nothing));
            }
            self.done += 1;
        }
        if let Some(step) = C::UP.get(at) {
            (self.progress)(Progress::Failed(*step, why.clone()));
        }
        self.done = C::UP.len();
        why
    }
}

#[cfg(test)]
mod tests;
