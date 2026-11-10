//! `real-run`: an Isoloom example generated for Docker by the local Isoloom's own command line,
//! then run for real by the ignored `real_run` test of `isothrum-cli` (up, status, check, park,
//! resume, down), the event stream kept under the build folder's `real-run/` (`target/real-run/`,
//! or `$CARGO_TARGET_DIR/real-run/`). `real-run cloud <cloud>` runs one on a cloud account
//! instead (see [`cloud`]).

mod cloud;

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Res, process, repo};

/// The arguments of `real-run`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The Isoloom example to run (default: hello-stack on Docker, solo-web on a cloud).
    #[arg(long, global = true)]
    example: Option<String>,
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

/// The example run on Docker when `--example` names none.
const EXAMPLE: &str = "hello-stack";

/// Generates the example for Docker with Isoloom's own command line, then runs the real-run test.
pub(crate) fn run(a: &Args) -> Res<()> {
    let root = repo::root();
    let isoloom = a.isoloom.clone().unwrap_or_else(|| root.join("../isoloom"));
    if let Some(On::Cloud(cloud)) = &a.on {
        return cloud::run(cloud, a.example.as_deref().unwrap_or(cloud::EXAMPLE), &isoloom);
    }
    let example = a.example.as_deref().unwrap_or(EXAMPLE);
    let out = repo::target_dir(&root).join("real-run");
    let run_dir = out.join(example);
    repo::remove(&run_dir)?;
    repo::copy_dir(&isoloom.join("examples").join(example), &run_dir).map_err(|e| format!("copying the example: {e}"))?;
    let bin = build_isoloom(&root, &isoloom)?;
    process::run(Command::new(bin).args(["generate", "--target", "docker"]).current_dir(&run_dir))?;
    let mut test = process::cargo(&root);
    test.args(["test", "-p", "isothrum-cli", "--test", "real_run", "--", "--ignored", "--nocapture"])
        .env("ISOTHRUM_REAL_RUN_DIR", &run_dir)
        .env("ISOTHRUM_REAL_EVENTS", out.join(format!("{example}.events.jsonl")));
    process::run(&mut test)
}

/// Builds Isoloom's command line from its own folder, into this checkout's build folder; its path.
fn build_isoloom(root: &Path, isoloom: &Path) -> Res<PathBuf> {
    let target = repo::target_dir(root).join("xtask-isoloom");
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::{Cli, Task};

    /// `real-run`'s arguments as the command line `args` gives them.
    fn parsed(args: &[&str]) -> Args {
        match Cli::parse_from([&["xtask", "real-run"], args].concat()).task {
            Task::RealRun(a) => a,
            other => panic!("not real-run: {other:?}"),
        }
    }

    #[test]
    fn one_example_flag_serves_docker_and_a_cloud() {
        assert_eq!(parsed(&[]).example, None);
        assert_eq!(parsed(&["--example", "web"]).example.as_deref(), Some("web"));
        let cloud = parsed(&["cloud", "aws", "--example", "web"]);
        assert_eq!(cloud.example.as_deref(), Some("web"), "given after the cloud, it is the run's");
        assert!(matches!(cloud.on, Some(On::Cloud(_))));
    }
}
