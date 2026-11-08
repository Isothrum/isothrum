//! Each command, on a runner the caller built.

use isothrum_runner::{BoxPick, Events, Occupied, Park, RunName, RunOptions, Runner, Running};

use crate::args::{Cli, Command, Mode, OpArgs, TargetArgs};
use crate::error::Result;
use crate::exit::Verdict;
use crate::term::{Human, Json, Term};
use crate::text::{human, status_text};
use crate::{locate, values};

/// Runs `cli` on `runner`, writing to `term`; `var` reads the environment (for secrets). An
/// operation's failure has been reported by its own `error` event, so it reads as
/// [`Verdict::Fail`]; what failed before any event is an error.
pub(crate) async fn run(cli: &Cli, runner: &Runner, term: &Term, var: &dyn Fn(&str) -> Option<String>) -> Result<Verdict> {
    let (json, human) = (Json(term), Human(term));
    let events: &dyn Events = if cli.json { &json } else { &human };
    match &cli.command {
        Command::Status(run) => status(cli.json, &open(runner, run)?, term).await,
        Command::Check(op) => check(cli.json, &open(runner, &op.run)?, &options(op, var)?, events, term).await,
        Command::Provision { op, machine } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(run.provision(machine.as_deref(), &options(op, var)?, events).await.is_ok()))
        }
        Command::Attach { op, machine, network, image } => {
            let pick = BoxPick {
                machine: machine.as_deref(),
                network: network.as_deref(),
                image: image.as_deref(),
            };
            attach(cli.json, &open(runner, &op.run)?, (&options(op, var)?, pick), events, term).await
        }
        Command::Detach { op, machine } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(run.detach_with(&options(op, var)?, machine.as_deref(), events).await.is_ok()))
        }
        Command::Shell { run, machine } => shell(cli.json, &open(runner, run)?, machine.as_deref(), term),
        Command::Events(run) => print_events(cli.json, runner, &open(runner, run)?, term),
        Command::Reap(args) => reap::run(args, runner, cli.json, term, var).await,
        Command::Key => {
            term.out(&runner.ssh_public_key().await?);
            Ok(Verdict::Pass)
        }
        Command::Up { .. } | Command::Down(_) | Command::Park { .. } | Command::Resume(_) | Command::Update(_) => {
            lifecycle(cli, runner, term, var, events).await
        }
    }
}

/// The commands that start, stop, park, resume or update the run (any other command is no
/// lifecycle change: it fails here, as it never comes here).
async fn lifecycle(cli: &Cli, runner: &Runner, term: &Term, var: &dyn Fn(&str) -> Option<String>, events: &dyn Events) -> Result<Verdict> {
    match &cli.command {
        Command::Up { op, replace, lifetime } => {
            let started = open(runner, &op.run)?.up(&start_opts(op, *replace, *lifetime, var)?, events).await;
            Ok(said(cli.json, started.ok(), "The run is up.", term))
        }
        Command::Down(op) => Ok(Verdict::of(open(runner, &op.run)?.down(&options(op, var)?, events).await.is_ok())),
        Command::Park { run, mode } => {
            let mode = match mode {
                Mode::Pause => Park::Pause,
                Mode::Shutdown => Park::Shutdown,
            };
            Ok(Verdict::of(open(runner, run)?.park(mode, events).await.is_ok()))
        }
        Command::Resume(op) => {
            let resumed = open(runner, &op.run)?.resume(&options(op, var)?, events).await;
            Ok(said(cli.json, resumed.ok(), "The run is up again.", term))
        }
        Command::Update(op) => {
            let updated = open(runner, &op.run)?.update(&options(op, var)?, events).await;
            Ok(said(cli.json, updated.ok(), "The run is up to date.", term))
        }
        _ => Ok(Verdict::Fail),
    }
}

/// A run that came up (a start, a resume, an update): its message, or `done`, unless the output is JSON
/// (its events said it); failed when it didn't come up.
fn said(json: bool, run: Option<isothrum_runner::Started>, done: &str, term: &Term) -> Verdict {
    let Some(run) = run else { return Verdict::Fail };
    if !json {
        term.out(run.message.as_deref().unwrap_or(done).trim_end());
    }
    Verdict::Pass
}

/// The run's status: JSON, or a short table.
async fn status(json: bool, run: &Running<'_>, term: &Term) -> Result<Verdict> {
    let status = run.status().await?;
    if json {
        term.out(&serde_json::to_string(&status)?);
    } else {
        for line in status_text(run.name().as_str(), &status) {
            term.out(&line);
        }
    }
    Ok(Verdict::Pass)
}

/// Hot-adds the on-demand access box, then says where to reach it (JSON, or a line).
async fn attach(json: bool, run: &Running<'_>, (opts, pick): (&RunOptions, BoxPick<'_>), events: &dyn Events, term: &Term) -> Result<Verdict> {
    let Ok(report) = run.attach_with(opts, pick, events).await else {
        return Ok(Verdict::Fail);
    };
    if json {
        term.out(&serde_json::to_string(&report)?);
    } else {
        let at = report.address.as_deref().map(|a| format!(" at {a}")).unwrap_or_default();
        term.out(&format!("Attached {}{at}.", report.machine));
    }
    Ok(Verdict::Pass)
}

/// The command that opens a shell into the access box: its argv, led by the variables it needs
/// (`VAR=value`, as a shell takes them), or the `ShellCommand` JSON.
fn shell(json: bool, run: &Running<'_>, machine: Option<&str>, term: &Term) -> Result<Verdict> {
    let cmd = run.shell_command(machine)?;
    if json {
        term.out(&serde_json::to_string(&cmd)?);
    } else {
        let env = cmd.env.iter().map(|(k, v)| format!("{k}={v}"));
        term.out(&env.chain(std::iter::once(cmd.program)).chain(cmd.args).collect::<Vec<_>>().join(" "));
    }
    Ok(Verdict::Pass)
}

/// The run's checks: a pass only when every one passed.
async fn check(json: bool, run: &Running<'_>, opts: &RunOptions, events: &dyn Events, term: &Term) -> Result<Verdict> {
    let Ok(report) = run.check(opts, events).await else {
        return Ok(Verdict::Fail);
    };
    if !json {
        let failed = report.results.iter().filter(|r| !r.ok).count();
        term.out(&format!("{} passed, {failed} failed", report.results.len() - failed));
    }
    Ok(Verdict::of(report.ok))
}

/// The run's recorded events, oldest first: JSON lines, or a line each for a person.
fn print_events(json: bool, runner: &Runner, run: &Running<'_>, term: &Term) -> Result<Verdict> {
    for event in runner.events(run.name())? {
        if json {
            term.out(&serde_json::to_string(&event)?);
        } else if let Some(line) = human(&event.kind) {
            term.out(&format!("{} {:<6} {line}", event.time, event.op.as_str()));
        }
    }
    Ok(Verdict::Pass)
}

/// The run `args` names.
fn open<'r>(runner: &'r Runner, args: &TargetArgs) -> Result<Running<'r>> {
    let manifest = locate::manifest(&args.path, args.target.as_deref())?;
    let name = args.name.as_deref().map(RunName::new).transpose()?;
    Ok(runner.open(&manifest, name)?)
}

/// The start options: `args`, plus `replace` (remove leftovers first) and a `lifetime` in minutes.
fn start_opts(args: &OpArgs, replace: bool, lifetime: Option<u64>, var: &dyn Fn(&str) -> Option<String>) -> Result<RunOptions> {
    let mut opts = options(args, var)?;
    if replace {
        opts = opts.with_occupied(Occupied::Replace);
    }
    if let Some(minutes) = lifetime {
        opts = opts.with_lifetime(std::time::Duration::from_secs(minutes.saturating_mul(60)));
    }
    Ok(opts)
}

/// The options `args` gives.
fn options(args: &OpArgs, var: &dyn Fn(&str) -> Option<String>) -> Result<RunOptions> {
    let mut opts = RunOptions::new()
        .with_params(values::params(&args.params)?)
        .with_secrets(values::secrets(&args.secrets, var)?);
    if let Some(label) = &args.host_label {
        opts = opts.with_host_label(label);
    }
    if let Some(provider) = &args.provider {
        opts = opts.with_provider(provider);
    }
    if let Some(jump) = &args.ssh_jump {
        opts = opts.with_ssh_jump(jump);
    }
    Ok(opts)
}

mod reap;
#[cfg(test)]
mod tests;
