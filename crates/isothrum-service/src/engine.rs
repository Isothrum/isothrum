//! What runs the operations: the [`Engine`] port, and [`RunnerEngine`], the Isothrum runner
//! behind it. The service knows nothing of the tools a run drives: it queues commands and hands
//! each to the engine.

use std::sync::Arc;

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_runner::store::OperationId;
use isothrum_runner::{BoxFuture, ErrorCode, Event, EventKind, Events, RunName, RunOptions, Runner, Running, SecretValue, Secrets, Status};

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

    /// The status of `run` (opened from `target`, else as its record says).
    fn status<'a>(&'a self, run: &'a RunName, target: Option<&'a Target>) -> BoxFuture<'a, Result<Status, Refusal>>;
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
            Command::Provision { machine, .. } => run.provision(machine.as_deref(), &opts, events).await,
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
            let opts = run_options(options).with_operation(operation);
            RunnerEngine::run(&running, command, opts, journal).await.unwrap_or(false)
        })
    }

    fn status<'a>(&'a self, run: &'a RunName, target: Option<&'a Target>) -> BoxFuture<'a, Result<Status, Refusal>> {
        Box::pin(async move {
            let running = match target {
                Some(target) => self.open(&Target {
                    name: Some(run.clone()),
                    ..target.clone()
                })?,
                None => self.runner.reopen(run)?,
            };
            Ok(running.status().await?)
        })
    }
}
