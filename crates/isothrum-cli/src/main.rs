//! `isothrum`: runs what Isoloom generates, from the command line. The imperative shell around
//! the runner: it reads the environment and the arguments once, builds the runner, races the
//! command against the signals that cancel it, and maps the outcome to an exit code.
//!
//! `args` is the command line's grammar and `commands` runs each command on the runner. What they
//! share: where things are (`locate`), the values an operation runs with (`values`), where the
//! command writes (`term`), what a person reads (`text`), the error that stops a command (`error`),
//! the signals that cancel it (`cancel`), the exit code it gives (`exit`) and its tracing (`log`).

mod args;
mod cancel;
mod commands;
mod error;
mod exit;
mod locate;
mod log;
#[cfg(test)]
mod reference;
mod term;
#[cfg(test)]
mod testing;
mod text;
mod values;

use std::process::ExitCode;

use clap::Parser;
use isothrum_runner::{EventKind, Runner, RunnerConfig};

use crate::args::{Cli, Command};
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::term::{Term, Trail};

#[tokio::main]
async fn main() -> ExitCode {
    let term = Term::std();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => return refused(&e, &term),
    };
    let var = |name: &str| std::env::var(name).ok();
    if let Some(filter) = var("ISOTHRUM_LOG")
        && let Err(e) = log::to_stderr(&filter, var("ISOTHRUM_LOG_FORMAT").as_deref())
    {
        term.err(&format!("warning: ISOTHRUM_LOG={filter}: {e}"));
    }
    let trail = Trail::default();
    let outcome = tokio::select! {
        outcome = start(&cli, &term, &trail, &var) => outcome,
        signal = cancelled(&term) => Err(Error::Cancelled(signal)),
    };
    let outcome = outcome.and_then(|verdict| term.finish().map(|()| verdict));
    if let Err(e) = &outcome {
        report(e, &cli, &term, &trail);
    }
    exit::code(&outcome)
}

/// Builds the runner this host and environment call for, then runs `cli`. Completions need no
/// runner: they touch no home.
async fn start(cli: &Cli, term: &Term, trail: &Trail, var: &dyn Fn(&str) -> Option<String>) -> Result<Verdict> {
    if let Command::Completions { shell } = &cli.command {
        term.out(&args::completions(*shell));
        return Ok(Verdict::Pass);
    }
    let user_home = std::env::home_dir();
    let home = locate::home(cli.home.clone(), var("ISOTHRUM_HOME"), user_home.clone())?;
    let runner = Runner::new(RunnerConfig::new(home))?;
    commands::run(cli, &runner, term, trail, var).await
}

/// The signal that cancels the command. One whose handler can't be installed is said, and the
/// command runs on without it.
async fn cancelled(term: &Term) -> cancel::Signal {
    match cancel::requested().await {
        Ok(signal) => signal,
        Err(e) => {
            term.err(&format!("warning: Ctrl-C and SIGTERM won't stop the command's tools: {e}"));
            std::future::pending().await
        }
    }
}

/// Says why the command stopped, unless its operation's own `error` event did: as an `error`
/// event with `--json` (the stream goes on from the last event), else as a line on standard
/// error.
fn report(e: &Error, cli: &Cli, term: &Term, trail: &Trail) {
    if trail.ended_in_error() {
        return;
    }
    if !cli.json {
        term.err(&format!("error: {e}"));
        return;
    }
    let kind = EventKind::Error {
        code: e.code(),
        message: e.to_string(),
        step: None,
        timeout: None,
    };
    let run = commands::run_named(&cli.command).map(str::to_owned);
    let event = trail.next(run, commands::op_of(&cli.command), kind);
    term.json(&event);
}

/// A command line clap refused: its help and version as clap prints them; an error as clap
/// prints it, or as an `error` event (`usage`) when the command line asks for JSON. Exit 2 (0
/// for help and version).
fn refused(e: &clap::Error, term: &Term) -> ExitCode {
    let json = std::env::args().any(|a| a == "--json");
    if !json || !e.use_stderr() {
        e.exit()
    }
    let kind = EventKind::Error {
        code: isothrum_runner::ErrorCode::Usage,
        message: e.render().to_string().trim_end().to_owned(),
        step: None,
        timeout: None,
    };
    term.json(&Trail::default().next(None, None, kind));
    ExitCode::from(exit::USAGE)
}
