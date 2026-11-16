//! Each command, on a runner the caller built.

mod reap;

use std::borrow::Borrow;

use isothrum_runner::{Endpoint, Events, NativeOpen, Op, Park, RunName, RunOptions, Runner, Running, ShellCommand};

use crate::args::{Cli, Command, Lifecycle, Mode, OpArgs, RunArgs, RunnerCommand, UpArgs};
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::term::{Human, Json, Term, Traced, Trail};
use crate::text::{human, status_text};
use crate::values;
use crate::values::Var;

/// Runs `cli`, writing to `term` (each event kept in `trail` too); `var` reads the environment
/// (for secrets). Every command but `completions` runs on the runner `runner` builds; completions
/// need none, so they touch no home. An operation's failure has been reported by its own `error`
/// event, so it reads as [`Verdict::Fail`]; what failed before any event is an error.
pub(crate) async fn run<R: Borrow<Runner>>(cli: &Cli, runner: impl FnOnce() -> Result<R>, term: &Term, trail: &Trail, var: &Var<'_>) -> Result<Verdict> {
    let command = match &cli.command {
        Command::Completions { shell } => {
            term.out(&crate::args::completions(*shell));
            return Ok(Verdict::Pass);
        }
        Command::Runner(command) => command,
        Command::Serve(_) => return Err(Error::Usage("`serve` runs the service, not a command on a runner".into())),
    };
    let runner = runner()?;
    let (json, human) = (Json(term), Human(term));
    let sink: &dyn Events = if cli.json { &json } else { &human };
    let traced = Traced { sink, trail };
    let out = Out {
        json: cli.json,
        term,
        events: &traced,
    };
    on_runner(command, runner.borrow(), &out, var).await
}

/// Runs `command` on `runner`, writing to `out`.
async fn on_runner(command: &RunnerCommand, runner: &Runner, out: &Out<'_>, var: &Var<'_>) -> Result<Verdict> {
    let Out { term, events, .. } = *out;
    match command {
        RunnerCommand::Status(run) => status(out, &open(runner, run)?).await,
        RunnerCommand::Exec { machine, op, argv } => exec(out, &open(runner, &op.run)?, machine, argv, &options(op, var)?).await,
        RunnerCommand::Provision { op, machine, with } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(
                run.provision(machine.as_deref(), with.as_deref(), &options(op, var)?, events).await.is_ok(),
            ))
        }
        RunnerCommand::Attach { op, machine } => {
            let run = open(runner, &op.run)?;
            Ok(Verdict::of(run.attach(machine, &options(op, var)?, events).await.is_ok()))
        }
        RunnerCommand::Console { run, machine, user } => print_command(out, &open(runner, run)?.console_command(machine, user.as_deref()).await?),
        RunnerCommand::Screen {
            machine,
            show_password,
            app,
            op,
        } => {
            let run = open(runner, &op.run)?;
            let opts = options(op, var)?;
            if *app {
                native(out, &run, machine, &opts).await
            } else {
                screen(out, &run, machine, &opts, *show_password).await
            }
        }
        RunnerCommand::Events(run) => print_events(out, runner, &open(runner, run)?),
        RunnerCommand::Reap(args) => reap::run(args, runner, out, var).await,
        RunnerCommand::Key => {
            let key = runner.ssh_public_key().await?;
            if out.json {
                term.out(&serde_json::json!({ "public_key": key }).to_string());
            } else {
                term.out(&key);
            }
            Ok(Verdict::Pass)
        }
        RunnerCommand::Lifecycle(command) => lifecycle(command, runner, out, var).await,
    }
}

/// The operation `command` runs, when it runs one.
pub(crate) fn op_of(command: &Command) -> Option<Op> {
    let Command::Runner(command) = command else { return None };
    Some(match &**command {
        RunnerCommand::Lifecycle(Lifecycle::Up(_)) => Op::Up,
        RunnerCommand::Lifecycle(Lifecycle::Down(_)) => Op::Down,
        RunnerCommand::Lifecycle(Lifecycle::Park { .. }) => Op::Park,
        RunnerCommand::Lifecycle(Lifecycle::Resume(_)) => Op::Resume,
        RunnerCommand::Lifecycle(Lifecycle::Update(_)) => Op::Update,
        RunnerCommand::Lifecycle(Lifecycle::Retire { .. }) => Op::Retire,
        RunnerCommand::Exec { .. } => Op::Check,
        RunnerCommand::Provision { .. } => Op::Provision,
        RunnerCommand::Attach { .. } => Op::Attach,
        RunnerCommand::Status(_)
        | RunnerCommand::Console { .. }
        | RunnerCommand::Screen { .. }
        | RunnerCommand::Events(_)
        | RunnerCommand::Reap(_)
        | RunnerCommand::Key => {
            return None;
        }
    })
}

/// The run `command` names with `--name`, when it does.
pub(crate) fn run_named(command: &Command) -> Option<&str> {
    let Command::Runner(command) = command else { return None };
    let target = match &**command {
        RunnerCommand::Status(run) | RunnerCommand::Console { run, .. } | RunnerCommand::Events(run) => run,
        RunnerCommand::Screen { op, .. } => &op.run,
        RunnerCommand::Reap(_) | RunnerCommand::Key => return None,
        operation => &op_args(operation)?.run,
    };
    target.name.as_deref()
}

/// The run and values of an operation command (`None` for the others).
pub(crate) fn op_args(command: &RunnerCommand) -> Option<&OpArgs> {
    match command {
        RunnerCommand::Lifecycle(
            Lifecycle::Up(UpArgs { op, .. })
            | Lifecycle::Down(op)
            | Lifecycle::Park { op, .. }
            | Lifecycle::Resume(op)
            | Lifecycle::Update(op)
            | Lifecycle::Retire { op, .. },
        )
        | RunnerCommand::Exec { op, .. }
        | RunnerCommand::Provision { op, .. }
        | RunnerCommand::Attach { op, .. } => Some(op),
        RunnerCommand::Status(_)
        | RunnerCommand::Console { .. }
        | RunnerCommand::Screen { .. }
        | RunnerCommand::Events(_)
        | RunnerCommand::Reap(_)
        | RunnerCommand::Key => None,
    }
}

/// Where a command writes: the terminal (JSON lines with `json`), and its operation's events.
pub(crate) struct Out<'a> {
    pub(crate) json: bool,
    pub(crate) term: &'a Term,
    pub(crate) events: &'a dyn Events,
}

/// A command that changes the run's life, written to `out`.
async fn lifecycle(command: &Lifecycle, runner: &Runner, out: &Out<'_>, var: &Var<'_>) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    match command {
        Lifecycle::Up(up) => {
            let started = open(runner, &up.op.run)?.up(&start_opts(up, var)?, events).await;
            Ok(report_came_up(json, started.ok().map(|s| s.outputs), "The run is up.", term))
        }
        Lifecycle::Down(op) => Ok(Verdict::of(open(runner, &op.run)?.down(&options(op, var)?, events).await.is_ok())),
        Lifecycle::Park { op, mode } => {
            let parked = open(runner, &op.run)?.park(park_mode(*mode), &options(op, var)?, events).await;
            Ok(Verdict::of(parked.is_ok()))
        }
        Lifecycle::Resume(op) => {
            let resumed = open(runner, &op.run)?.resume(&options(op, var)?, events).await;
            Ok(report_came_up(json, resumed.ok().map(|()| Vec::new()), "The run is up again.", term))
        }
        Lifecycle::Update(op) => {
            let updated = open(runner, &op.run)?.update(&options(op, var)?, events).await;
            Ok(report_came_up(json, updated.ok().map(|()| Vec::new()), "The run is up to date.", term))
        }
        Lifecycle::Retire { op, machine } => {
            let retired = open(runner, &op.run)?.retire(machine, &options(op, var)?, events).await;
            Ok(Verdict::of(retired.is_ok()))
        }
    }
}

/// How `mode` parks a run.
pub(crate) fn park_mode(mode: Mode) -> Park {
    match mode {
        Mode::Pause => Park::Pause,
        Mode::Shutdown => Park::Shutdown,
    }
}

/// A run that came up (a start, a resume, an update): `done` and its outputs, unless the output
/// is JSON (its events said it); failed when it didn't come up.
fn report_came_up(json: bool, outputs: Option<Vec<isothrum_runner::OutputValue>>, done: &str, term: &Term) -> Verdict {
    let Some(outputs) = outputs else { return Verdict::Fail };
    if !json {
        term.out(done);
        for o in &outputs {
            if let Some(value) = &o.value {
                term.out(&format!("{}: {value}", o.name));
            }
        }
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

/// Runs `argv` in `machine`: what it printed, on the command's own streams (in its `result` with
/// `--json`), and a pass only when it exited 0.
async fn exec(out: &Out<'_>, run: &Running<'_>, machine: &str, argv: &[String], opts: &RunOptions) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    let Ok(report) = run.exec(machine, argv, opts, events).await else {
        return Ok(Verdict::Fail);
    };
    if !json {
        if !report.stdout.is_empty() {
            term.out(report.stdout.trim_end());
        }
        if !report.stderr.is_empty() {
            term.err(report.stderr.trim_end());
        }
    }
    Ok(Verdict::of(report.exit == 0))
}

/// `machine`'s graphical console, reached: where it answers (its password only when `show`), as
/// one JSON object or lines for a person. A console reached through an SSH tunnel keeps it open
/// until the viewer is done: Ctrl-C (the command's cancellation) ends it, and a tunnel that closes
/// on its own (its machine gone, the connection dropped, a `down`) ends the command, with exit 1.
async fn screen(out: &Out<'_>, run: &Running<'_>, machine: &str, opts: &RunOptions, show: bool) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
    let mut console = run.graphical_console(machine, opts).await?;
    let tunnel = console.tunnel.as_ref().map(|t| t.via().to_owned());
    let protocol = isothrum_runner::wire_name(&console.protocol);
    let (address, websocket) = match &console.endpoint {
        Endpoint::Loopback(at) => (Some(at.to_string()), None),
        Endpoint::ProxmoxVnc(vnc) => (None, Some(websocket_shown(vnc, show))),
        _ => (None, None),
    };
    let password = console.password.as_ref().filter(|_| show).map(|p| p.expose().to_owned());
    if json {
        let line = serde_json::json!({
            "machine": console.machine, "protocol": protocol, "address": address, "websocket": websocket,
            "user": console.user, "password": password, "note": console.note, "tunnel": tunnel,
        });
        term.out(&line.to_string());
    } else {
        term.out(&format!("{protocol} {}", address.or(websocket).unwrap_or_default()));
        if let Some(user) = &console.user {
            term.out(&format!("user: {user}"));
        }
        match (&password, &console.password) {
            (Some(p), _) => term.out(&format!("password: {p}")),
            (None, Some(_)) => term.out("password: (hidden: --show-password prints it)"),
            (None, None) => {}
        }
        if let Some(note) = &console.note {
            term.out(&format!("note: {note}"));
        }
    }
    if let Some(open) = console.tunnel.as_mut() {
        term.err(&format!(
            "The tunnel through {} stays open while this runs: Ctrl-C closes it once the viewer is done.",
            open.via()
        ));
        let said = open.closed().await;
        term.err(&format!("The tunnel closed: {said}."));
        return Ok(Verdict::Fail);
    }
    Ok(Verdict::Pass)
}

/// What opens `machine` in its hypervisor's own app (`screen --app`): each way, best first, as
/// JSON or one line each.
async fn native(out: &Out<'_>, run: &Running<'_>, machine: &str, opts: &RunOptions) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
    let console = run.native_console(machine, opts).await?;
    let ways: Vec<serde_json::Value> = console
        .ways
        .iter()
        .map(|way| match way {
            NativeOpen::Program { program, args } => serde_json::json!({ "program": program, "args": args }),
            NativeOpen::MacApp { app, args } => serde_json::json!({ "mac_app": app, "args": args }),
            NativeOpen::Url { url } => serde_json::json!({ "url": url }),
            _ => serde_json::Value::Null,
        })
        .collect();
    if json {
        term.out(&serde_json::json!({ "machine": console.machine, "app": console.app, "ways": ways }).to_string());
    } else {
        term.out(&format!("{}:", console.app));
        for way in &console.ways {
            match way {
                NativeOpen::Program { program, args } => term.out(&format!(
                    "  {}",
                    words(std::iter::once(program.as_str()).chain(args.iter().map(String::as_str)))
                )),
                NativeOpen::MacApp { app, args } => {
                    let app = app.to_string_lossy();
                    term.out(&format!("  {}", words(["open", "-a", &app].into_iter().chain(args.iter().map(String::as_str)))));
                }
                NativeOpen::Url { url } => term.out(&format!("  {url}")),
                _ => {}
            }
        }
    }
    Ok(Verdict::Pass)
}

/// `words` as one POSIX shell line, each quoted when it holds anything but letters, digits and
/// `-_./:=@%+,`.
fn words<'a>(words: impl IntoIterator<Item = &'a str>) -> String {
    let plain = |w: &str| !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    words
        .into_iter()
        .map(|w| if plain(w) { w.to_owned() } else { format!("'{}'", w.replace('\'', "'\\''")) })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A Proxmox VE console's websocket as `screen` prints it: its ticket (the console's password, in
/// its query) masked unless `show` (`--show-password`).
fn websocket_shown(vnc: &isothrum_runner::ProxmoxVnc, show: bool) -> String {
    ticket_shown(vnc.websocket.expose(), vnc.ticket.expose(), show)
}

/// `url` with `ticket` in it (as it is, or percent-encoded) masked unless `show`.
fn ticket_shown(url: &str, ticket: &str, show: bool) -> String {
    const HIDDEN: &str = "<hidden: --show-password prints it>";
    if show || ticket.is_empty() {
        return url.to_owned();
    }
    url.replace(&isothrum_runner::percent_encoded(ticket), HIDDEN).replace(ticket, HIDDEN)
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

/// The run's recorded events, oldest first: JSON lines, or a line each for a person.
fn print_events(out: &Out<'_>, runner: &Runner, run: &Running<'_>) -> Result<Verdict> {
    let Out { json, term, .. } = *out;
    let log = runner.events(run.name())?;
    for event in log.events {
        if json {
            term.out(&serde_json::to_string(&event)?);
        } else if let Some(line) = human(&event.kind) {
            let op = event.op.map_or("", Op::as_str);
            term.out(&format!("{} {op:<6} {line}", event.time));
        }
    }
    if log.unreadable > 0 {
        term.err(&format!(
            "warning: {} line(s) of run `{}`'s event log couldn't be read (written by another version of Isothrum, or cut short)",
            log.unreadable,
            run.name()
        ));
    }
    Ok(Verdict::Pass)
}

/// The run `args` names: the project in its folder, of the format `--format` names, else the one
/// its files show.
fn open<'r>(runner: &'r Runner, args: &RunArgs) -> Result<Running<'r>> {
    // A malformed `--name` is a malformed value: a usage error, as clap's own are.
    let name = args
        .name
        .as_deref()
        .map(RunName::new)
        .transpose()
        .map_err(|e| Error::Usage(format!("--name: {e}")))?;
    Ok(match args.format {
        Some(format) => runner.open_as(&args.path, format.into(), name)?,
        None => runner.open(&args.path, name)?,
    })
}

/// The start options `up` gives: its operation's, plus a `lifetime` in minutes.
fn start_opts(up: &UpArgs, var: &Var<'_>) -> Result<RunOptions> {
    let mut opts = options(&up.op, var)?;
    if let Some(minutes) = up.lifetime {
        opts = opts.with_lifetime(std::time::Duration::from_secs(minutes.saturating_mul(60)));
    }
    Ok(opts)
}

/// The options `args` gives.
fn options(args: &OpArgs, var: &Var<'_>) -> Result<RunOptions> {
    let mut opts = RunOptions::new().with_secrets(values::secrets(&args.secrets, var)?);
    for (name, value) in values::env(&args.env)? {
        opts = opts.with_env(name, value);
    }
    if let Some(label) = &args.host_label {
        opts = opts.with_host_label(label);
    }
    if let Some(provider) = args.provider {
        opts = opts.with_provider(provider);
    }
    Ok(opts)
}

#[cfg(test)]
mod tests;
