//! `mutants [CRATE...]`: mutation testing (cargo-mutants) of the compiler, and of the
//! runner's executor and the pure parts of its drivers (what reads Compose's, Vagrant's and
//! Terraform's answers). Each mutant is a small change to the code (an operator swapped, a condition
//! negated, a function returning a default) and must make a test of its crate fail. A mutant no
//! test catches ("missed") fails the run, until a test catches it or `.cargo/mutants.toml` lists
//! it, with its reason, as one no test can tell apart from the original.
//!
//! Each crate's report goes to `<target dir>/mutants/<crate>/mutants.out` (`missed.txt`,
//! `caught.txt`, `timeout.txt`, `outcomes.json`). `--jobs N` builds and tests N mutants at once,
//! each in its own copy of the tree. Isoloom's `cargo xtask check-all --deep` runs it. Needs
//! `cargo install --locked cargo-mutants`.

use std::process::ExitCode;

use crate::{Res, process, repo};

/// The crates mutated by default, each with the files mutated in it (every file when none).
const SCOPE: [(&str, &[&str]); 2] = [("isothrum-compiler", &[]), ("isothrum-runner", &RUNNER_FILES)];

/// The runner's files mutated: the executor, and what reads the engines' answers.
const RUNNER_FILES: [&str; 6] = [
    "crates/isothrum-runner/src/executor.rs",
    "crates/isothrum-runner/src/executor/**",
    "crates/isothrum-runner/src/driver/compose/config.rs",
    "crates/isothrum-runner/src/driver/compose/ps.rs",
    "crates/isothrum-runner/src/driver/vagrant/status.rs",
    "crates/isothrum-runner/src/driver/terraform/state.rs",
];

/// The arguments of `mutants`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// How many mutants are built and tested at once.
    #[arg(long, short, default_value_t = 1)]
    jobs: u8,
    /// The crates (default: isothrum-compiler, and isothrum-runner's executor and drivers' readers).
    crates: Vec<String>,
}

/// The files mutated in `krate`: its scope's, or every one for a crate outside it.
fn files_of(krate: &str) -> &'static [&'static str] {
    SCOPE.iter().find(|(name, _)| *name == krate).map_or(&[], |(_, files)| files)
}

/// Mutates each crate in turn; fails when a mutant survived or timed out in any.
pub(crate) fn run(a: &Args) -> Res<ExitCode> {
    let root = repo::root();
    process::require_cargo_tool(&root, "mutants")?;
    let crates: Vec<String> = if a.crates.is_empty() {
        SCOPE.iter().map(|(name, _)| (*name).to_owned()).collect()
    } else {
        a.crates.clone()
    };
    let mut clean = true;
    for krate in &crates {
        let out = repo::target_dir(&root).join("mutants").join(krate);
        // cargo-mutants makes `mutants.out` in this folder, not the folder itself.
        std::fs::create_dir_all(&out)?;
        let mut cmd = process::cargo(&root);
        cmd.args(["mutants", "--package", krate, "--jobs", &a.jobs.to_string(), "--output"]).arg(&out);
        for file in files_of(krate) {
            cmd.args(["--file", file]);
        }
        let caught = process::passed(&mut cmd)?;
        clean &= caught;
        let verdict = if caught { "every mutant caught" } else { "mutants missed or timed out" };
        println!("mutants: {krate}: {verdict} (report: {})", out.join("mutants.out").display());
    }
    Ok(process::exit_code(clean))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runner_is_mutated_in_its_executor_and_readers_and_the_compiler_whole() {
        let runner = files_of("isothrum-runner");
        assert_eq!(runner, RUNNER_FILES);
        for file in runner {
            assert!(repo::root().join(file.trim_end_matches("/**")).exists(), "{file} is gone: update the scope");
        }
        let whole: &[&str] = &[];
        assert_eq!(files_of("isothrum-compiler"), whole);
        assert_eq!(files_of("isothrum-cli"), whole, "a crate named on the command line is mutated whole");
    }
}
