//! The commands that go through the home's runner service (every operation, and `status`): the
//! command line is then a client, the service runs the operation, and the command prints its
//! events as they come. Ctrl-C asks the service to cancel the operation and waits for it to end
//! (a second Ctrl-C leaves it to the service).

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_client::{Client, Followed};
use isothrum_runner::{EventKind, ResultData, RunName};

use crate::args::{Cli, Command as CliCommand, Lifecycle, OpArgs, RunArgs, RunnerCommand, UpArgs};
use crate::cancel::{self, Signal};
use crate::commands::{Out, op_args, park_mode};
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::locate;
use crate::text::status_text;
use crate::values::{self, Var};

/// The variable that runs every command in-process, as `--no-service` does.
const NO_SERVICE: &str = "ISOTHRUM_NO_SERVICE";

/// The service command `cli` is, when it goes through the service: an operation or `status`,
/// unless `--no-service` or `ISOTHRUM_NO_SERVICE` says to run it here.
pub(crate) fn command<'c>(cli: &'c Cli, var: &Var<'_>) -> Option<&'c RunnerCommand> {
    let in_process = cli.no_service || var(NO_SERVICE).is_some_and(|v| !v.is_empty() && v != "0");
    let CliCommand::Runner(command) = &cli.command else { return None };
    let through_service = matches!(
        **command,
        RunnerCommand::Lifecycle(_) | RunnerCommand::Status(_) | RunnerCommand::Exec { .. } | RunnerCommand::Provision { .. } | RunnerCommand::Attach { .. }
    );
    (!in_process && through_service).then_some(&**command)
}

/// What a finished operation prints for a person.
#[derive(Debug, Clone, Copy)]
enum Shown {
    /// A run that came up: this, then its outputs.
    Started(&'static str),
    /// Nothing more than its events (an `exec`'s output is among them).
    Plain,
}

/// Runs `command` through the service of the home `cli` names, writing to `out`.
pub(crate) async fn run(cli: &Cli, command: &RunnerCommand, out: &Out<'_>, var: &Var<'_>) -> Result<Verdict> {
    let home = locate::home(cli.home.clone(), var("ISOTHRUM_HOME"), std::env::home_dir())?;
    if let RunnerCommand::Status(run) = command {
        let target = target(run)?;
        let name = match &target.name {
            Some(name) => name.clone(),
            None => RunName::from_folder(&target.path)?,
        };
        let client = Client::connect(&home, None).await?;
        let status = client.status(&name, Some(target)).await?;
        if out.json {
            out.term.out(&serde_json::to_string(&status)?);
        } else {
            for line in status_text(name.as_str(), &status) {
                out.term.out(&line);
            }
        }
        return Ok(Verdict::Pass);
    }
    let (wire, shown) = operation(command, var)?;
    let client = Client::connect(&home, None).await?;
    let followed = client.follow(wire).await?;
    follow(&client, followed, out, shown).await
}

/// The service's command for the operation `command`, and what its end prints.
fn operation(command: &RunnerCommand, var: &Var<'_>) -> Result<(Command, Shown)> {
    let op = op_args(command).ok_or_else(|| Error::Usage("not an operation".into()))?;
    let (target, options) = (target(&op.run)?, options(op, var)?);
    Ok(match command {
        RunnerCommand::Lifecycle(Lifecycle::Up(UpArgs { lifetime, .. })) => {
            let lifetime_s = lifetime.map(|minutes| minutes.saturating_mul(60));
            (Command::Up { target, options, lifetime_s }, Shown::Started("The run is up."))
        }
        RunnerCommand::Lifecycle(Lifecycle::Down(_)) => (Command::Down { target, options }, Shown::Plain),
        RunnerCommand::Lifecycle(Lifecycle::Park { mode, .. }) => {
            let mode = park_mode(*mode);
            (Command::Park { target, options, mode }, Shown::Plain)
        }
        RunnerCommand::Lifecycle(Lifecycle::Resume(_)) => (Command::Resume { target, options }, Shown::Plain),
        RunnerCommand::Lifecycle(Lifecycle::Update(_)) => (Command::Update { target, options }, Shown::Plain),
        RunnerCommand::Lifecycle(Lifecycle::Retire { machine, .. }) => {
            let machine = machine.clone();
            (Command::Retire { target, options, machine }, Shown::Plain)
        }
        RunnerCommand::Exec { machine, argv, .. } => {
            let (machine, argv) = (machine.clone(), argv.clone());
            (
                Command::Exec {
                    target,
                    options,
                    machine,
                    argv,
                },
                Shown::Plain,
            )
        }
        RunnerCommand::Provision { machine, .. } => {
            let machine = machine.clone();
            (Command::Provision { target, options, machine }, Shown::Plain)
        }
        RunnerCommand::Attach { machine, .. } => {
            let machine = machine.clone();
            (Command::Attach { target, options, machine }, Shown::Plain)
        }
        _ => return Err(Error::Usage("not an operation".into())),
    })
}

/// The run `args` names: its project's folder, absolute (the service has another working
/// folder), its format when given, and its `--name`.
fn target(args: &RunArgs) -> Result<Target> {
    // A malformed `--name` is a malformed value: a usage error, as clap's own are.
    let name = args
        .name
        .as_deref()
        .map(RunName::new)
        .transpose()
        .map_err(|e| Error::Usage(format!("--name: {e}")))?;
    let path = std::path::absolute(&args.path).map_err(|source| Error::Read {
        path: args.path.clone(),
        source,
    })?;
    let target = Target::new(path, name);
    Ok(match args.format {
        Some(format) => target.with_format(format.into()),
        None => target,
    })
}

/// The values `args` gives, for the wire.
fn options(args: &OpArgs, var: &Var<'_>) -> Result<OpOptions> {
    let secrets = values::secrets(&args.secrets, var)?;
    Ok(OpOptions {
        provider: args.provider,
        env: values::env(&args.env)?,
        secrets: secrets.iter().map(|(k, v)| (k.to_owned(), v.expose().to_owned())).collect(),
        host_label: args.host_label.clone(),
    })
}

/// The first signal that asks the command to stop; never, when no handler could be installed.
async fn signalled() -> Signal {
    match cancel::requested().await {
        Ok(signal) => signal,
        Err(_) => std::future::pending().await,
    }
}

/// Prints the followed operation's events to its end, and what it ended with.
async fn follow(client: &Client, mut followed: Followed, out: &Out<'_>, shown: Shown) -> Result<Verdict> {
    let accepted = followed.accepted.clone();
    if accepted.position > 0 && !out.json {
        out.term
            .err(&format!("Queued behind {} operation(s) on run `{}`…", accepted.position, accepted.run));
    }
    let mut cancelled: Option<Signal> = None;
    let mut ending = Ending::default();
    let mut next_signal = Box::pin(signalled());
    loop {
        let event = tokio::select! {
            event = followed.next() => event,
            asked = &mut next_signal => {
                next_signal = Box::pin(signalled());
                if cancelled.is_some() {
                    return Err(Error::Cancelled(asked));
                }
                out.term.err("Cancelling the operation: the service stops its tools (Ctrl-C again to leave it to the service)…");
                if let Err(e) = client.cancel(accepted.operation).await {
                    tracing::debug!(error = %e, "the operation had ended");
                }
                cancelled = Some(asked);
                continue;
            }
        };
        let Some(event) = event else { break };
        let event = event?;
        ending.see(&event.kind);
        out.events.emit(&event);
    }
    if let Some(signal) = cancelled {
        return Err(Error::Cancelled(signal));
    }
    Ok(ending.verdict(out, shown))
}

/// What the operation's events said of its end: its `result`, whether it reached its goal and
/// its data.
#[derive(Debug, Default)]
struct Ending(Option<(bool, ResultData)>);

impl Ending {
    /// Notes `kind`.
    fn see(&mut self, kind: &EventKind) {
        if let EventKind::Result { ok, data } = kind {
            self.0 = Some((*ok, data.clone()));
        }
    }

    /// The verdict, and what a person reads of it.
    fn verdict(self, out: &Out<'_>, shown: Shown) -> Verdict {
        let Some((ok, data)) = self.0 else { return Verdict::Fail };
        if let (false, Shown::Started(done)) = (out.json, shown) {
            out.term.out(done);
            if let ResultData::Started { outputs, .. } = &data {
                for o in outputs {
                    if let Some(value) = &o.value {
                        out.term.out(&format!("{}: {value}", o.name));
                    }
                }
            }
        }
        Verdict::of(ok)
    }
}
