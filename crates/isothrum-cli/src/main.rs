//! `isothrum`: runs what Isoloom generates, from the command line. The imperative shell around
//! the runner: it reads the environment and the arguments once, builds the runner, and maps the
//! outcome to an exit code.
//!
//! `args` is the command line's grammar and `commands` runs each command on the runner. What they
//! share: where things are (`locate`), the values an operation runs with (`values`), where the
//! command writes (`term`), what a person reads (`text`), the error that stops a command (`error`)
//! and the exit code it gives (`exit`).

mod args;
mod commands;
mod error;
mod exit;
mod locate;
mod term;
#[cfg(test)]
mod testing;
mod text;
mod values;

use std::process::ExitCode;

use clap::Parser;
use isothrum_runner::{Runner, RunnerConfig};

use crate::args::Cli;
use crate::error::Result;
use crate::exit::Verdict;
use crate::term::Term;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let term = Term::std();
    let var = |name: &str| std::env::var(name).ok();
    if let Some(level) = var("ISOTHRUM_LOG")
        && let Err(e) = trace_to_stderr(&level)
    {
        term.err(&format!("warning: ISOTHRUM_LOG={level}: {e}"));
    }
    let outcome = start(&cli, &term, &var).await;
    let outcome = outcome.and_then(|verdict| term.finish().map(|()| verdict));
    if let Err(e) = &outcome {
        term.err(&format!("error: {e}"));
    }
    exit::code(&outcome)
}

/// The runner's tracing (its runs, steps, drivers and processes) on standard error, at `level`
/// (`info`, `debug`, `trace`).
fn trace_to_stderr(level: &str) -> std::result::Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let level: tracing::Level = level.parse()?;
    tracing_subscriber::fmt().with_max_level(level).with_writer(std::io::stderr).try_init()
}

/// Builds the runner this host and environment call for, then runs `cli`.
async fn start(cli: &Cli, term: &Term, var: &dyn Fn(&str) -> Option<String>) -> Result<Verdict> {
    let user_home = std::env::home_dir();
    let home = locate::home(cli.home.clone(), var("ISOTHRUM_HOME"), user_home.clone())?;
    let runner = Runner::new(RunnerConfig::new(home))?;
    commands::run(cli, &runner, term, var).await
}
