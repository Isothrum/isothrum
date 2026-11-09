//! Each command, on a runner the caller built.

use isothrum_runner::{AccessPick, Events, Occupied, Park, RunName, RunOptions, Runner, Running, ShellCommand};

use crate::args::{Cli, Command, Lifecycle, Mode, OpArgs, TargetArgs};
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
    let out = Out { json: cli.json, term, events };
    match &cli.command {
        Command::Status(run) => status(&out, &open(runner, run)?).await,
        Command::Check(op) => check(&out, &open(runner, &op.run)?, &options(op, var)?).await,
        Command::Provision { op, machine } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(run.provision(machine.as_deref(), &options(op, var)?, events).await.is_ok()))
        }
        Command::Attach { op, machine, network, image } => {
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
            attach(&out, &open(runner, &op.run)?, pick, &options(op, var)?).await
        }
        Command::Detach { op, machine } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(run.detach(machine.as_deref(), &options(op, var)?, events).await.is_ok()))
        }
        Command::Console { run, machine } => print_command(&out, &open(runner, run)?.console_command(machine)?),
        Command::Events(run) => print_events(&out, runner, &open(runner, run)?),
        Command::Reap(args) => reap::run(args, runner, &out, var).await,
        Command::Key => {
            term.out(&runner.ssh_public_key().await?);
            Ok(Verdict::Pass)
        }
        Command::Lifecycle(command) => lifecycle(command, runner, &out, var).await,
    }
}

/// Where a command writes: the terminal (JSON lines with `json`), and its operation's events.
struct Out<'a> {
    json: bool,
    term: &'a Term,
    events: &'a dyn Events,
}

/// A command that changes the run's life, written to `out`.
async fn lifecycle(command: &Lifecycle, runner: &Runner, out: &Out<'_>, var: &dyn Fn(&str) -> Option<String>) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    match command {
        Lifecycle::Up {
            op,
            replace,
            lifetime,
            ssh_jump,
        } => {
            let started = open(runner, &op.run)?
                .up(&start_opts(op, *replace, *lifetime, ssh_jump.as_deref(), var)?, events)
                .await;
            Ok(said(json, started.ok(), "The run is up.", term))
        }
        Lifecycle::Down(op) => Ok(Verdict::of(open(runner, &op.run)?.down(&options(op, var)?, events).await.is_ok())),
        Lifecycle::Park { op, mode } => {
            let parked = open(runner, &op.run)?.park(park_mode(*mode), &options(op, var)?, events).await;
            Ok(Verdict::of(parked.is_ok()))
        }
        Lifecycle::Resume(op) => {
            let resumed = open(runner, &op.run)?.resume(&options(op, var)?, events).await;
            Ok(said(json, resumed.ok(), "The run is up again.", term))
        }
        Lifecycle::Update(op) => {
            let updated = open(runner, &op.run)?.update(&options(op, var)?, events).await;
            Ok(said(json, updated.ok(), "The run is up to date.", term))
        }
        Lifecycle::Retire { op, machine } => {
            let retired = open(runner, &op.run)?.retire(machine, &options(op, var)?, events).await;
            Ok(Verdict::of(retired.is_ok()))
        }
    }
}

/// How `mode` parks a run.
fn park_mode(mode: Mode) -> Park {
    match mode {
        Mode::Pause => Park::Pause,
        Mode::Shutdown => Park::Shutdown,
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
async fn status(out: &Out<'_>, run: &Running<'_>) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
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

/// Hot-adds the on-demand access machine, then says where to reach it (JSON, or a line).
async fn attach(out: &Out<'_>, run: &Running<'_>, pick: AccessPick<'_>, opts: &RunOptions) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    let Ok(report) = run.attach(pick, opts, events).await else {
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

/// The command that opens a console into a machine (`console`): one line to paste in a POSIX
/// shell ([`ShellCommand::sh_line`]: its folder, its variables, each word quoted as it needs), or
/// the `ShellCommand` JSON.
fn print_command(out: &Out<'_>, cmd: &ShellCommand) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
    if json {
        term.out(&serde_json::to_string(cmd)?);
    } else {
        term.out(&cmd.sh_line());
    }
    Ok(Verdict::Pass)
}

/// The run's checks: a pass only when every one passed.
async fn check(out: &Out<'_>, run: &Running<'_>, opts: &RunOptions) -> Result<Verdict> {
    let Out { json, term, events } = *out;
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
fn print_events(out: &Out<'_>, runner: &Runner, run: &Running<'_>) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
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

/// The start options: `args`, plus `replace` (remove leftovers first), a `lifetime` in minutes
/// and the SSH jump the run records.
fn start_opts(args: &OpArgs, replace: bool, lifetime: Option<u64>, ssh_jump: Option<&str>, var: &dyn Fn(&str) -> Option<String>) -> Result<RunOptions> {
    let mut opts = options(args, var)?;
    if let Some(jump) = ssh_jump {
        opts = opts.with_ssh_jump(jump);
    }
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
    Ok(opts)
}

mod reap;
#[cfg(test)]
mod tests;
