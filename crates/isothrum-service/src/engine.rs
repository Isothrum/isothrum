//! What runs the operations: the [`Engine`] port, and [`RunnerEngine`], the Isothrum runner
//! behind it. The service knows nothing of the tools a run drives: it queues commands and hands
//! each to the engine.

use std::sync::Arc;

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_runner::store::OperationId;
use isothrum_runner::{
    BoxFuture, Console, ErrorCode, Event, EventKind, Events, Format, InputVariable, NativeConsole, Project, RunName, RunOptions, Runner, Running, SecretValue,
    Secrets, ServiceEndpoint, ShellCommand, Status,
};
use isothrum_runner::{Download, DownloadRemoval, DownloadToAdopt, DownloadsToRemove, MachineUsage};

/// Why the engine refused a request: the runner's report of it (boxed: it is large, and rare). A
/// refusal never changed a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub Box<isothrum_runner::ErrorReport>);

impl Refusal {
    /// A refusal of code `code`.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Refusal {
        Refusal(Box::new(isothrum_runner::ErrorReport::new(code, message).unchanged()))
    }

    /// Its code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.0.code
    }
}

impl From<isothrum_runner::Error> for Refusal {
    fn from(e: isothrum_runner::Error) -> Refusal {
        Refusal(Box::new(e.report().unchanged()))
    }
}

/// Where an operation's events go. As [`Events`], it is told of each event already kept in the
/// home's store (the runner keeps its own); [`Journal::keep`] keeps one the engine writes itself.
pub trait Journal: Events {
    /// Keeps `event`, one the engine wrote itself, as the operation's, and tells the subscribers.
    fn keep(&self, event: Event);
}

/// What runs the service's operations and answers the status reads. It keeps every event of an
/// operation in the home's store.
pub trait Engine: Send + Sync + 'static {
    /// The run `target` names, checked enough to queue an operation on it (its name).
    ///
    /// # Errors
    ///
    /// The run can't be opened there.
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal>;

    /// Runs the operation `command` asks for on `run`, as the journal's queued operation
    /// `operation`, its events to `journal`: exactly one `result` or `error` among them, unless
    /// the future is dropped (a cancel), when the engine writes what it can. Returns whether it
    /// reached its goal.
    fn execute<'a>(&'a self, run: &'a RunName, command: &'a Command, operation: OperationId, journal: &'a dyn Journal) -> BoxFuture<'a, bool>;

    /// The status of `run` (opened from `target`, else as its record says), its read's tools
    /// given `options`' values and secrets.
    fn status<'a>(&'a self, run: &'a RunName, target: Option<&'a Target>, options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>>;

    /// The recorded runs whose lifetime passed and that may still hold something, each as the
    /// folder its `down` opens: the service's reaper removes them. By default, none.
    ///
    /// # Errors
    ///
    /// The records can't be read.
    fn expired(&self) -> Result<Vec<Target>, Refusal> {
        Ok(Vec::new())
    }

    /// The input variables the Terraform folder `target` declares.
    ///
    /// # Errors
    ///
    /// The folder isn't a Terraform module; by default, an engine that reads no folder.
    fn variables(&self, target: &Target) -> Result<Vec<InputVariable>, Refusal> {
        let _ = target;
        Err(Refusal::new(ErrorCode::Unsupported, "this engine reads no folder's variables"))
    }

    /// The command that opens a console into `machine` of the run `at` names, as `user` when
    /// given; by default, none.
    fn console_command<'a>(&'a self, at: Opened<'a>, machine: &'a str, user: Option<&'a str>) -> BoxFuture<'a, Result<ShellCommand, Refusal>> {
        let _ = (at, machine, user);
        Box::pin(async { Err(unsupported("consoles")) })
    }

    /// `machine`'s graphical console, its tunnel (when it has one) the caller's to keep; by
    /// default, none.
    fn graphical_console<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<Console, Refusal>> {
        let _ = (at, options, machine);
        Box::pin(async { Err(unsupported("graphical consoles")) })
    }

    /// What opens running `machine` in its hypervisor's own app; by default, nothing.
    fn native_console<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<NativeConsole, Refusal>> {
        let _ = (at, options, machine);
        Box::pin(async { Err(unsupported("hypervisor apps")) })
    }

    /// The service endpoints the run publishes, each where this computer reaches it (a forward
    /// the engine keeps open); by default, none.
    fn service_endpoints<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, Refusal>> {
        let _ = (at, options);
        Box::pin(async { Err(unsupported("service endpoints")) })
    }

    /// What each running machine of the run uses now; by default, nothing.
    fn usage<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<MachineUsage>, Refusal>> {
        let _ = (at, options);
        Box::pin(async { Err(unsupported("machine usage")) })
    }

    /// The public half of the engine's SSH key, one line; by default, none.
    fn public_key(&self) -> BoxFuture<'_, Result<String, Refusal>> {
        Box::pin(async { Err(unsupported("SSH keys")) })
    }

    /// What the runs' own downloads fetched, as the home records them; by default, none.
    fn downloads(&self) -> BoxFuture<'_, Result<Vec<Download>, Refusal>> {
        Box::pin(async { Err(unsupported("recorded downloads")) })
    }

    /// Removes the recorded downloads `which` names, each on its own; by default, none.
    fn remove_downloads<'a>(&'a self, which: &'a DownloadsToRemove) -> BoxFuture<'a, Result<Vec<DownloadRemoval>, Refusal>> {
        let _ = which;
        Box::pin(async { Err(unsupported("recorded downloads")) })
    }

    /// Adopts `adopted` into the home's record of downloads; by default, none.
    fn adopt_downloads<'a>(&'a self, adopted: &'a [DownloadToAdopt]) -> BoxFuture<'a, Result<Vec<Download>, Refusal>> {
        let _ = adopted;
        Box::pin(async { Err(unsupported("recorded downloads")) })
    }

    /// The address blocks the engine's networks hold, but `except`'s own; by default, none asked.
    fn address_blocks_in_use<'a>(&'a self, except: Option<&'a RunName>, docker_context: Option<&'a str>) -> BoxFuture<'a, Result<Vec<String>, Refusal>> {
        let _ = (except, docker_context);
        Box::pin(async { Err(unsupported("address blocks")) })
    }
}

/// A run as a query names it: by name, opened from `target` when given, else as its record says.
#[derive(Debug, Clone, Copy)]
pub struct Opened<'a> {
    /// The run.
    pub run: &'a RunName,
    /// Where it is opened from.
    pub target: Option<&'a Target>,
}

/// The refusal of an engine that has no `what`.
fn unsupported(what: &str) -> Refusal {
    Refusal::new(ErrorCode::Unsupported, format!("this engine has no {what}"))
}

/// The Isothrum runner as the service's engine.
#[derive(Debug, Clone)]
pub struct RunnerEngine {
    runner: Arc<Runner>,
}

impl RunnerEngine {
    /// The engine over `runner`.
    #[must_use]
    pub fn new(runner: Arc<Runner>) -> RunnerEngine {
        RunnerEngine { runner }
    }

    /// The run `target` names: its folder, as its format (recognized there unless given).
    fn open(&self, target: &Target) -> Result<Running<'_>, Refusal> {
        let opened = match target.format {
            Some(format) => self.runner.open_as(&target.path, format, target.name.clone()),
            None => self.runner.open(&target.path, target.name.clone()),
        };
        Ok(opened?)
    }

    /// The run `at` names, opened from its target under its name, else as recorded.
    fn reopen(&self, at: Opened<'_>) -> Result<Running<'_>, Refusal> {
        match at.target {
            Some(target) => self.open(&Target {
                name: Some(at.run.clone()),
                ..target.clone()
            }),
            None => Ok(self.runner.reopen(at.run)?),
        }
    }

    /// Runs `command` on the opened `run`: whether it reached its goal (a command `exec` ran that
    /// exited non-zero didn't); Err: it failed (its `error` event said why).
    async fn run(run: &Running<'_>, command: &Command, opts: RunOptions, events: &dyn Events) -> isothrum_runner::Result<bool> {
        match command {
            Command::Up { lifetime_s, .. } => {
                let opts = match lifetime_s {
                    Some(secs) => opts.with_lifetime(std::time::Duration::from_secs(*secs)),
                    None => opts,
                };
                run.up(&opts, events).await.map(drop)
            }
            Command::Down { .. } => run.down(&opts, events).await,
            Command::Park { mode, .. } => run.park(*mode, &opts, events).await,
            Command::Resume { .. } => run.resume(&opts, events).await,
            Command::Update { .. } => run.update(&opts, events).await,
            Command::Provision { machine, with, .. } => run.provision(machine.as_deref(), with.as_deref(), &opts, events).await,
            Command::Attach { machine, .. } => run.attach(machine, &opts, events).await,
            Command::Retire { machine, .. } => run.retire(machine, &opts, events).await,
            Command::Exec { machine, argv, .. } => return run.exec(machine, argv, &opts, events).await.map(|report| report.exit == 0),
            // `execute` takes operations only.
            _ => Ok(()),
        }
        .map(|()| true)
    }
}

/// The runner's options `options` give.
fn run_options(options: &OpOptions) -> RunOptions {
    let secrets: Secrets = options.secrets.iter().map(|(k, v)| (k.clone(), SecretValue::new(v.clone()))).collect();
    let mut opts = RunOptions::new().with_secrets(secrets);
    for (name, value) in &options.env {
        opts = opts.with_env(name, value);
    }
    if let Some(label) = &options.host_label {
        opts = opts.with_host_label(label);
    }
    if let Some(provider) = options.provider {
        opts = opts.with_provider(provider);
    }
    if let Some(context) = &options.docker_context {
        opts = opts.with_docker_context(context);
    }
    if options.reveal {
        opts = opts.with_reveal();
    }
    opts
}

impl Engine for RunnerEngine {
    fn resolve(&self, target: &Target) -> Result<RunName, Refusal> {
        Ok(self.open(target)?.name().clone())
    }

    fn execute<'a>(&'a self, run: &'a RunName, command: &'a Command, operation: OperationId, journal: &'a dyn Journal) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            let Some((target, options)) = command.target() else {
                return false;
            };
            let named = Target {
                name: Some(run.clone()),
                ..target.clone()
            };
            let running = match self.open(&named) {
                Ok(running) => running,
                Err(refusal) => {
                    // Refused before the runner began: its `error` is written here.
                    let kind = EventKind::Error(*refusal.0);
                    journal.keep(Event::new(1, Some(run.to_string()), command.op(), kind));
                    return false;
                }
            };
            let opts = run_options(options).with_operation(operation);
            RunnerEngine::run(&running, command, opts, journal).await.unwrap_or(false)
        })
    }

    fn expired(&self) -> Result<Vec<Target>, Refusal> {
        let records = self.runner.expired(std::time::SystemTime::now())?;
        Ok(records
            .into_iter()
            .map(|r| Target::new(r.project, Some(r.name)).with_format(r.format))
            .collect())
    }

    fn variables(&self, target: &Target) -> Result<Vec<InputVariable>, Refusal> {
        let format = target.format.unwrap_or(Format::Terraform);
        Ok(Project::open(&target.path, Some(format))?.variables()?)
    }

    fn status<'a>(&'a self, run: &'a RunName, target: Option<&'a Target>, options: &'a OpOptions) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async move { Ok(self.reopen(Opened { run, target })?.status_with(&run_options(options)).await?) })
    }

    fn console_command<'a>(&'a self, at: Opened<'a>, machine: &'a str, user: Option<&'a str>) -> BoxFuture<'a, Result<ShellCommand, Refusal>> {
        Box::pin(async move { Ok(self.reopen(at)?.console_command(machine, user).await?) })
    }

    fn graphical_console<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<Console, Refusal>> {
        Box::pin(async move { Ok(self.reopen(at)?.graphical_console(machine, &run_options(options)).await?) })
    }

    fn native_console<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions, machine: &'a str) -> BoxFuture<'a, Result<NativeConsole, Refusal>> {
        Box::pin(async move { Ok(self.reopen(at)?.native_console(machine, &run_options(options)).await?) })
    }

    fn service_endpoints<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<ServiceEndpoint>, Refusal>> {
        Box::pin(async move { Ok(self.reopen(at)?.service_endpoints(&run_options(options)).await?) })
    }

    fn usage<'a>(&'a self, at: Opened<'a>, options: &'a OpOptions) -> BoxFuture<'a, Result<Vec<MachineUsage>, Refusal>> {
        Box::pin(async move { Ok(self.reopen(at)?.usage(&run_options(options)).await?) })
    }

    fn public_key(&self) -> BoxFuture<'_, Result<String, Refusal>> {
        Box::pin(async move { Ok(self.runner.ssh_public_key().await?) })
    }

    fn downloads(&self) -> BoxFuture<'_, Result<Vec<Download>, Refusal>> {
        Box::pin(async move { Ok(self.runner.downloads().await?) })
    }

    fn remove_downloads<'a>(&'a self, which: &'a DownloadsToRemove) -> BoxFuture<'a, Result<Vec<DownloadRemoval>, Refusal>> {
        Box::pin(async move { Ok(self.runner.remove_downloads(which).await?) })
    }

    fn adopt_downloads<'a>(&'a self, adopted: &'a [DownloadToAdopt]) -> BoxFuture<'a, Result<Vec<Download>, Refusal>> {
        Box::pin(async move { Ok(self.runner.adopt_downloads(adopted).await?) })
    }

    fn address_blocks_in_use<'a>(&'a self, except: Option<&'a RunName>, docker_context: Option<&'a str>) -> BoxFuture<'a, Result<Vec<String>, Refusal>> {
        Box::pin(async move { Ok(self.runner.address_blocks_in_use(except, docker_context).await?) })
    }
}
