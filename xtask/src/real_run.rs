//! `real-run`: an Isoloom example generated for Docker by the local Isoloom's own command line,
//! then run for real by the ignored `real_run` test of `isothrum-cli` (up, status, check, park,
//! resume, down), the event stream kept under `target/real-run/`. `real-run cloud <cloud>` runs
//! one on a cloud account instead (see [`cloud`]).

mod cloud;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Res, process, repo};

/// The arguments of `real-run`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The Isoloom example to run.
    #[arg(long, default_value = "hello-stack")]
    example: String,
    /// The Isoloom checkout (default: `../isoloom` beside this repository).
    #[arg(long, global = true)]
    isoloom: Option<PathBuf>,
    /// A cloud account instead of this host's Docker.
    #[command(subcommand)]
    on: Option<On>,
}

/// Where else a real run goes.
#[derive(Debug, clap::Subcommand)]
enum On {
    /// An Isoloom example (solo-web) on a cloud account, with the credentials in the environment:
    /// up, status, check, then down and reap whatever happened.
    Cloud(cloud::CloudArgs),
}

/// Generates the example for Docker with Isoloom's own command line, then runs the real-run test.
pub(crate) fn run(a: &Args) -> Res<()> {
    let root = repo::root();
    let isoloom = a.isoloom.clone().unwrap_or_else(|| root.join("../isoloom"));
    if let Some(On::Cloud(cloud)) = &a.on {
        return cloud::run(cloud, &isoloom);
    }
    let out = root.join("target").join("real-run");
    let run_dir = out.join(&a.example);
    repo::remove(&run_dir)?;
    repo::copy_dir(&isoloom.join("examples").join(&a.example), &run_dir).map_err(|e| format!("copying the example: {e}"))?;
    let bin = build_isoloom(&root, &isoloom)?;
    process::run(Command::new(bin).args(["generate", "--target", "docker"]).current_dir(&run_dir))?;
    let mut test = process::cargo(&root);
    test.args(["test", "-p", "isothrum-cli", "--test", "real_run", "--", "--ignored", "--nocapture"])
        .env("ISOTHRUM_REAL_RUN_DIR", &run_dir)
        .env("ISOTHRUM_REAL_EVENTS", out.join(format!("{}.events.jsonl", a.example)));
    process::run(&mut test)
}

/// Builds Isoloom's command line from its own folder, into this checkout's build folder; its path.
fn build_isoloom(root: &Path, isoloom: &Path) -> Res<PathBuf> {
    let target = root.join("target").join("xtask-isoloom");
    // From Isoloom's own folder: Cargo reads the configuration of the folder it runs in, and this
    // repository's (a dev-link) points the resolution at a lockfile of its own.
    let mut build = process::cargo(isoloom);
    build
        .args(["build", "-p", "isoloom", "--manifest-path"])
        .arg(isoloom.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target);
    process::run(&mut build)?;
    Ok(target.join("debug").join(if cfg!(windows) { "isoloom.exe" } else { "isoloom" }))
}
