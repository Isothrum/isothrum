//! The commands that go through the home's runner service (every operation, and `status`): the
//! command line is then a client, the service runs the operation, and the command prints its
//! events as they come. Ctrl-C asks the service to cancel the operation and waits for it to end
//! (a second Ctrl-C leaves it to the service).

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_client::{Client, Followed};
use isothrum_runner::{EventKind, RunName, wire_name};

use crate::args::{Cli, Command as CliCommand, Lifecycle, OpArgs, RunArgs, RunnerCommand};
use crate::cancel::{self, Signal};
use crate::commands::{Out, park_mode};
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
        RunnerCommand::Lifecycle(_)
            | RunnerCommand::Status(_)
            | RunnerCommand::Check(_)
            | RunnerCommand::Provision { .. }
            | RunnerCommand::Attach { .. }
            | RunnerCommand::Detach { .. }
    );
    (!in_process && through_service).then_some(&**command)
}

/// What a finished operation prints for a person.
#[derive(Debug, Clone, Copy)]
enum Shown {
    /// A run that came up: its message, else this.
    Started(&'static str),
    /// The machine attached, and where.
    Attached,
    /// How many checks passed and failed.
    Checked,
    /// Nothing more than its events.
    Plain,
}

/// Runs `command` through the service of the home `cli` names, writing to `out`.
pub(crate) async fn run(cli: &Cli, command: &RunnerCommand, out: &Out<'_>, var: &Var<'_>) -> Result<Verdict> {
    let home = locate::home(cli.home.clone(), var("ISOTHRUM_HOME"), std::env::home_dir())?;
    if let RunnerCommand::Status(run) = command {
        let (name, path) = (run_name(run)?, manifest(run)?);
        let client = Client::connect(&home, None).await?;
        let status = client.status(&name, Some(&path)).await?;
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
        RunnerCommand::Lifecycle(Lifecycle::Up {
            replace, lifetime, ssh_jump, ..
        }) => {
            let options = OpOptions {
                ssh_jump: ssh_jump.clone(),
                ..options
            };
            let lifetime_s = lifetime.map(|minutes| minutes.saturating_mul(60));
            let fresh = *replace;
            (
                Command::Up {
                    target,
                    options,
                    fresh,
                    lifetime_s,
                },
                Shown::Started("The run is up."),
            )
        }
        RunnerCommand::Lifecycle(Lifecycle::Down(_)) => (Command::Down { target, options }, Shown::Plain),
        RunnerCommand::Lifecycle(Lifecycle::Park { mode, .. }) => {
            let mode = park_mode(*mode);
            (Command::Park { target, options, mode }, Shown::Plain)
        }
        RunnerCommand::Lifecycle(Lifecycle::Resume(_)) => (Command::Resume { target, options }, Shown::Started("The run is up again.")),
        RunnerCommand::Lifecycle(Lifecycle::Update(_)) => (Command::Update { target, options }, Shown::Started("The run is up to date.")),
        RunnerCommand::Lifecycle(Lifecycle::Retire { machine, .. }) => {
            let machine = machine.clone();
            (Command::Retire { target, options, machine }, Shown::Plain)
        }
        RunnerCommand::Check(_) => (Command::Check { target, options }, Shown::Checked),
        RunnerCommand::Provision { machine, .. } => {
            let machine = machine.clone();
            (Command::Provision { target, options, machine }, Shown::Plain)
        }
        RunnerCommand::Attach { machine, network, image, .. } => {
            let (machine, network, image) = (machine.clone(), network.clone(), image.clone());
            (
                Command::Attach {
                    target,
                    options,
                    machine,
                    network,
                    image,
                },
                Shown::Attached,
            )
        }
        RunnerCommand::Detach { machine, .. } => {
            let machine = machine.clone();
            (Command::Detach { target, options, machine }, Shown::Plain)
        }
        _ => return Err(Error::Usage("not an operation".into())),
    })
}

/// The run and values of an operation command.
fn op_args(command: &RunnerCommand) -> Option<&OpArgs> {
    match command {
        RunnerCommand::Lifecycle(
            Lifecycle::Up { op, .. }
            | Lifecycle::Down(op)
            | Lifecycle::Park { op, .. }
            | Lifecycle::Resume(op)
            | Lifecycle::Update(op)
            | Lifecycle::Retire { op, .. },
        )
        | RunnerCommand::Check(op)
        | RunnerCommand::Provision { op, .. }
        | RunnerCommand::Attach { op, .. }
        | RunnerCommand::Detach { op, .. } => Some(op),
        _ => None,
    }
}

/// The run `args` names: its manifest, absolute (the service has another working folder), and
/// its `--name`.
fn target(args: &RunArgs) -> Result<Target> {
    let name = args.name.as_deref().map(parse_name).transpose()?;
    Ok(Target::new(manifest(args)?, name))
}

/// The manifest `args` names, absolute.
fn manifest(args: &RunArgs) -> Result<std::path::PathBuf> {
    let manifest = locate::manifest(&args.path, args.target.as_deref())?;
    std::path::absolute(&manifest).map_err(|source| Error::Read { path: manifest, source })
}

/// `--name`'s value as a run's name (a malformed one is a usage error, as clap's own are).
fn parse_name(name: &str) -> Result<RunName> {
    RunName::new(name).map_err(|e| Error::Usage(format!("--name: {e}")))
}

/// The run's name `args` gives: `--name`, else its manifest's environment's.
fn run_name(args: &RunArgs) -> Result<RunName> {
    if let Some(name) = &args.name {
        return parse_name(name);
    }
    let loaded = isothrum_runner::load_manifest(&manifest(args)?)?;
    Ok(RunName::new(loaded.manifest.environment.name.as_str())?)
}

/// The values `args` gives, for the wire.
fn options(args: &OpArgs, var: &Var<'_>) -> Result<OpOptions> {
    let secrets = values::secrets(&args.secrets, var)?;
    Ok(OpOptions {
        provider: args.provider.map(|p| wire_name(&p)),
        params: values::params(&args.params)?,
        secrets: secrets.iter().map(|(k, v)| (k.to_owned(), v.expose().to_owned())).collect(),
        host_label: args.host_label.clone(),
        ssh_jump: None,
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
        let Some(event) = event?.to_runner() else { continue };
        ending.see(&event.kind);
        out.events.emit(&event);
    }
    if let Some(signal) = cancelled {
        return Err(Error::Cancelled(signal));
    }
    Ok(ending.verdict(out, shown))
}

/// What the operation's events said of its end.
#[derive(Debug, Default)]
struct Ending {
    /// Its `result`: whether it reached its goal, and its data.
    result: Option<(bool, serde_json::Value)>,
    passed: usize,
    failed: usize,
}

impl Ending {
    /// Notes `kind`.
    fn see(&mut self, kind: &EventKind) {
        match kind {
            EventKind::Result { ok, data } => self.result = Some((*ok, data.clone())),
            EventKind::Check { ok: true, .. } => self.passed += 1,
            EventKind::Check { ok: false, .. } => self.failed += 1,
            _ => {}
        }
    }

    /// The verdict, and what a person reads of it.
    fn verdict(self, out: &Out<'_>, shown: Shown) -> Verdict {
        let Some((ok, data)) = self.result else { return Verdict::Fail };
        if !out.json {
            match shown {
                Shown::Started(done) => out.term.out(text(&data, "message").unwrap_or(done).trim_end()),
                Shown::Attached => {
                    let machine = text(&data, "machine").unwrap_or_default();
                    let at = text(&data, "address").map(|a| format!(" at {a}")).unwrap_or_default();
                    out.term.out(&format!("Attached {machine}{at}."));
                }
                Shown::Checked => out.term.out(&format!("{} passed, {} failed", self.passed, self.failed)),
                Shown::Plain => {}
            }
        }
        Verdict::of(ok)
    }
}

/// The text of field `name` of a `result`'s data, when it has one.
fn text<'d>(data: &'d serde_json::Value, name: &str) -> Option<&'d str> {
    data.get(name).and_then(serde_json::Value::as_str)
}
