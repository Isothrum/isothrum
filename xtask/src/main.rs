//! Isothrum's development tasks, run as `cargo run -p xtask -- <task>`, the same on macOS, Linux
//! and Windows:
//!
//! - `schema`: the JSON Schemas of the event stream and of a run's status, written to
//!   `crates/isothrum-runner/schema/`;
//! - `real-run`: an Isoloom example generated for Docker with the local Isoloom, then the ignored
//!   real-run test against it (up, status, check, park, resume, down), the event stream kept
//!   under `target/real-run/`;
//! - `quality`: the ratchet, the clean-code counts per crate against `quality-baseline.json`;
//! - `test`: the tests under cargo-nextest, then the doctests, in a temp folder of their own that
//!   must be empty at the end (`ISOTHRUM_KEEP_TEMP=1` keeps it);
//! - `msrv` and `features`: every crate checked with its declared minimum Rust, and with every
//!   combination of its features (cargo-hack);
//! - `mutants`: mutation testing of the runner's readers of what Compose, Vagrant and Terraform
//!   answer (its adapters, cargo-mutants).
//!
//! CONTRIBUTING lists them with the other gates. Each task is a module; `process` runs the tools they drive, `repo` finds the checkout.
//! `workspace` holds the tests that keep the workspace's dependencies one-way and its manifests inheriting the workspace's;
//! `ci_scripts` those of the workflows' scripts.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the tasks report on the terminal: printing is their interface, not stray debugging output"
)]
#![expect(
    clippy::disallowed_methods,
    reason = "the xtask drives the tools of the gates (cargo, git, the hooks' scripts) itself: it is no part of the runner"
)]

#[cfg(all(test, unix))]
mod ci_scripts;
#[cfg(test)]
mod guardrails;
mod hack;
mod mutants;
mod process;
mod quality;
mod real_run;
mod repo;
mod schema;
mod test;
#[cfg(test)]
mod workspace;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// What a task returns: its exit code, or the error printed as `xtask: ...` (exit code 2).
type Res<T> = Result<T, Box<dyn std::error::Error>>;

/// Isothrum's development tasks.
#[derive(Debug, Parser)]
#[command(name = "xtask", bin_name = "cargo run -p xtask --", about = "Isothrum's development tasks.")]
struct Cli {
    #[command(subcommand)]
    task: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Writes the JSON Schemas of the event stream and of a run's status.
    Schema,
    /// Runs an Isoloom example on this host's Docker through the `isothrum` command line.
    RealRun(real_run::Args),
    /// The ratchet: every clean-code count per crate against quality-baseline.json; fails when
    /// one goes up (`--update` writes them when they only went down).
    Quality(quality::Args),
    /// The workspace's tests under cargo-nextest (`.config/nextest.toml`), then the doctests; fails
    /// when they leave anything in their temp folder (`ISOTHRUM_KEEP_TEMP=1` keeps it to look into).
    Test(test::Args),
    /// Every crate checked with the toolchain its `rust-version` names (cargo-hack).
    Msrv,
    /// Every crate checked with every combination of its features (cargo-hack).
    Features,
    /// Mutation testing of the runner's adapters (its readers of the tools' answers)
    /// (cargo-mutants): exits 1 when a mutant survives.
    Mutants(mutants::Args),
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Res<ExitCode> {
    match cli.task {
        Task::Schema => schema::write().map(|()| ExitCode::SUCCESS),
        Task::RealRun(a) => real_run::run(&a).map(|()| ExitCode::SUCCESS),
        Task::Quality(a) => quality::run(&a),
        Task::Test(a) => test::run(&a),
        Task::Msrv => hack::msrv(),
        Task::Features => hack::features(),
        Task::Mutants(a) => mutants::run(&a),
    }
}
