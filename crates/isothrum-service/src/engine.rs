//! What runs the operations: the [`Engine`] port, and [`RunnerEngine`], the Isothrum runner
//! behind it. The service knows nothing of the tools a run drives: it queues commands and hands
//! each to the engine.

use std::path::Path;
use std::sync::Arc;

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_runner::isoloom_manifest::Provider;
use isothrum_runner::store::OperationId;
use isothrum_runner::{
    AccessPick, BoxFuture, ErrorCode, Event, EventKind, Events, Occupied, RunName, RunOptions, Runner, Running, SecretValue, Secrets, Status, from_wire_name,
};

/// Why the engine refused a request: a code and a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The code.
    pub code: ErrorCode,
    /// The message.
    pub message: String,
}

impl Refusal {
    /// A refusal of code `code`.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Refusal {
        Refusal { code, message: message.into() }
    }
}

impl From<isothrum_runner::Error> for Refusal {
    fn from(e: isothrum_runner::Error) -> Refusal {
        Refusal::new(e.code(), e.to_string())
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

    /// The status of `run` (opened from `path`, else from where its record says).
    fn status<'a>(&'a self, run: &'a RunName, path: Option<&'a Path>) -> BoxFuture<'a, Result<Status, Refusal>>;
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

    /// The run `target` names.
    fn open(&self, target: &Target) -> Result<Running<'_>, Refusal> {
        Ok(self.runner.open(&target.path, target.name.clone())?)
    }

    /// Runs `command` on the opened `run`: whether it reached its goal (a check that failed
    /// didn't); Err: it failed (its `error` event said why).
    async fn run(run: &Running<'_>, command: &Command, opts: RunOptions, events: &dyn Events) -> isothrum_runner::Result<bool> {
        match command {
            Command::Up { fresh, lifetime_s, .. } => run.up(&start_options(opts, *fresh, *lifetime_s), events).await.map(drop),
            Command::Down { .. } => run.down(&opts, events).await,
            Command::Park { mode, .. } => run.park(*mode, &opts, events).await,
            Command::Resume { .. } => run.resume(&opts, events).await.map(drop),
            Command::Update { .. } => run.update(&opts, events).await.map(drop),
            Command::Check { .. } => return run.check(&opts, events).await.map(|report| report.ok),
            Command::Provision { machine, .. } => run.provision(machine.as_deref(), &opts, events).await,
            Command::Attach { machine, network, image, .. } => {
                let pick = access_pick(machine.as_deref(), network.as_deref(), image.as_deref());
                run.attach(pick, &opts, events).await.map(drop)
            }
            Command::Detach { machine, .. } => run.detach(machine.as_deref(), &opts, events).await,
            Command::Retire { machine, .. } => run.retire(machine, &opts, events).await,
            // `execute` takes operations only.
            _ => Ok(()),
        }
        .map(|()| true)
    }
}

/// `opts` for a start: `fresh` removes whatever exists first; `lifetime_s` bounds the run's life.
fn start_options(opts: RunOptions, fresh: bool, lifetime_s: Option<u64>) -> RunOptions {
    let opts = if fresh { opts.with_occupied(Occupied::Replace) } else { opts };
    match lifetime_s {
        Some(secs) => opts.with_lifetime(std::time::Duration::from_secs(secs)),
        None => opts,
    }
}

/// The access machine an attach adds, where, from what.
fn access_pick<'a>(machine: Option<&'a str>, network: Option<&'a str>, image: Option<&'a str>) -> AccessPick<'a> {
    let mut pick = AccessPick::default();
    if let Some(m) = machine {
        pick = pick.with_machine(m);
    }
    if let Some(n) = network {
        pick = pick.with_network(n);
    }
    if let Some(i) = image {
        pick = pick.with_image(i);
    }
    pick
}

/// The runner's options `options` give.
///
/// # Errors
///
/// A provider the runner doesn't know.
fn run_options(options: &OpOptions) -> Result<RunOptions, Refusal> {
    let secrets: Secrets = options.secrets.iter().map(|(k, v)| (k.clone(), SecretValue::new(v.clone()))).collect();
    let mut opts = RunOptions::new().with_params(options.params.clone()).with_secrets(secrets);
    if let Some(label) = &options.host_label {
        opts = opts.with_host_label(label);
    }
    if let Some(name) = &options.provider {
        let provider =
            from_wire_name::<Provider>(name).ok_or_else(|| Refusal::new(ErrorCode::Usage, format!("`{name}` isn't a provider this runner knows")))?;
        opts = opts.with_provider(provider);
    }
    if let Some(jump) = &options.ssh_jump {
        opts = opts.with_ssh_jump(jump);
    }
    Ok(opts)
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
            let opened = self
                .open(&Target::new(&target.path, Some(run.clone())))
                .and_then(|r| Ok((r, run_options(options)?.with_operation(operation))));
            let (running, opts) = match opened {
                Ok(opened) => opened,
                Err(refusal) => {
                    // Refused before the runner began: its `error` is written here.
                    let kind = EventKind::Error {
                        code: refusal.code,
                        message: refusal.message,
                        step: None,
                        timeout: None,
                    };
                    journal.keep(Event::new(1, Some(run.to_string()), command.op(), kind));
                    return false;
                }
            };
            RunnerEngine::run(&running, command, opts, journal).await.unwrap_or(false)
        })
    }

    fn status<'a>(&'a self, run: &'a RunName, path: Option<&'a Path>) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async move {
            let path = if let Some(path) = path {
                path.to_path_buf()
            } else {
                let record = self.runner.run_record(run)?;
                record
                    .map(|r| r.manifest)
                    .ok_or_else(|| Refusal::new(ErrorCode::NotRunning, format!("no run `{run}` is recorded: say where it is")))?
            };
            let running = self.open(&Target::new(path, Some(run.clone())))?;
            Ok(running.status().await?)
        })
    }
}
