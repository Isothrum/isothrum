//! `check`: every check a change to Isothrum must pass, in this repository, one command
//! (`cargo xtask check`; `--fast` for the short list a branch runs while it is worked on). Each
//! step is named and runs whatever the others did; the end is one line per step and a non-zero
//! exit when any is red.
//!
//! A step whose tool isn't installed (cargo-deny, cargo-machete, typos, cargo-semver-checks,
//! cargo-nextest, the 1.90 toolchain, the Windows target and its C compiler) is red, saying what to
//! install: a check that didn't run proves nothing, so it never passes for one that did.

use std::path::Path;
use std::process::{Command, ExitCode};

use crate::{Res, process, repo};

/// The arguments of `check`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The short list only: fetch, fmt, clippy, the tests, and clippy for Windows.
    #[arg(long)]
    fast: bool,
}

/// How a step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    /// It passed.
    Ok,
    /// It failed.
    Red,
    /// Its tool isn't there: what to install.
    Missing(String),
}

/// What a step needs installed beyond cargo, and how to get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    /// Nothing more.
    Nothing,
    /// A cargo subcommand (`cargo <name> --version` answers).
    CargoTool(&'static str),
    /// A program on `PATH` (`<name> --version` answers).
    Program(&'static str),
    /// A rustup toolchain.
    Toolchain(&'static str),
    /// A rustup target, and a C compiler for it on `PATH` (the bundled SQLite's build script).
    Target(&'static str, &'static [&'static str]),
}

/// One step: its name, whether the short list has it, what it needs, and what it runs.
struct Step {
    name: &'static str,
    fast: bool,
    needs: Needs,
    run: fn(&Path) -> Res<bool>,
}

/// The Windows target the Windows clippy checks, and the C compilers it may build SQLite with.
const WINDOWS: (&str, &[&str]) = ("x86_64-pc-windows-gnu", &["x86_64-w64-mingw32-gcc", "x86_64-w64-mingw32-clang"]);

/// The minimum toolchain, the workspace's `rust-version`.
const MSRV: &str = "1.90";

/// Every step, in the order they run.
const STEPS: [Step; 14] = [
    Step {
        name: "fetch",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["fetch"]),
    },
    Step {
        name: "isoloom-free",
        fast: false,
        needs: Needs::Nothing,
        run: isoloom_free,
    },
    Step {
        name: "fmt",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["fmt", "--all", "--check"]),
    },
    Step {
        name: "clippy",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
    },
    Step {
        name: "test",
        fast: true,
        needs: Needs::CargoTool("nextest"),
        run: |r| cargo(r, &["run", "-p", "xtask", "-q", "--", "test"]),
    },
    Step {
        name: "doc",
        fast: false,
        needs: Needs::Nothing,
        run: doc,
    },
    Step {
        name: "quality",
        fast: false,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["run", "-p", "xtask", "-q", "--", "quality"]),
    },
    Step {
        name: "regenerate",
        fast: false,
        needs: Needs::Nothing,
        run: |r| process::passed(Command::new("sh").arg(".githooks/regenerate").arg("--check").current_dir(r)),
    },
    Step {
        name: "typos",
        fast: false,
        needs: Needs::Program("typos"),
        run: |r| process::passed(Command::new("typos").current_dir(r)),
    },
    Step {
        name: "machete",
        fast: false,
        needs: Needs::CargoTool("machete"),
        run: |r| cargo(r, &["machete"]),
    },
    Step {
        name: "deny",
        fast: false,
        needs: Needs::CargoTool("deny"),
        run: |r| cargo(r, &["deny", "--all-features", "check"]),
    },
    Step {
        name: "msrv",
        fast: false,
        needs: Needs::Toolchain(MSRV),
        run: msrv,
    },
    Step {
        name: "windows-gnu",
        fast: true,
        needs: Needs::Target(WINDOWS.0, WINDOWS.1),
        run: |r| cargo(r, &["clippy", "--workspace", "--all-targets", "--target", WINDOWS.0, "--", "-D", "warnings"]),
    },
    Step {
        name: "semver",
        fast: false,
        needs: Needs::CargoTool("semver-checks"),
        run: semver,
    },
];

/// Runs the steps `fast` asks for, each whatever the others did, then says how each ended.
pub(crate) fn run(a: &Args) -> ExitCode {
    let root = repo::root();
    let mut ended = Vec::new();
    for step in STEPS.iter().filter(|s| s.fast || !a.fast) {
        println!("=== check {}", step.name);
        let outcome = match missing(&root, step.needs) {
            Some(install) => Outcome::Missing(install),
            None => match (step.run)(&root) {
                Ok(true) => Outcome::Ok,
                Ok(false) => Outcome::Red,
                Err(e) => {
                    eprintln!("check {}: {e}", step.name);
                    Outcome::Red
                }
            },
        };
        ended.push((step.name, outcome));
    }
    let (lines, green) = summary(&ended);
    for line in lines {
        println!("{line}");
    }
    process::exit_code(green)
}

/// One line per step (`fmt: ok`, `deny: red (missing: cargo install --locked cargo-deny)`), and
/// whether every step passed: a missing tool is red.
fn summary(ended: &[(&str, Outcome)]) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = ended
        .iter()
        .map(|(name, outcome)| match outcome {
            Outcome::Ok => format!("check: {name}: ok"),
            Outcome::Red => format!("check: {name}: red"),
            Outcome::Missing(install) => format!("check: {name}: red (not installed: {install})"),
        })
        .collect();
    let green = ended.iter().all(|(_, o)| *o == Outcome::Ok);
    lines.push(format!("check: {}", if green { "green" } else { "red" }));
    (lines, green)
}

/// What to install for `needs`, when it isn't there.
fn missing(root: &Path, needs: Needs) -> Option<String> {
    let answers = |cmd: &mut Command| process::first_line(cmd).is_some();
    match needs {
        Needs::Nothing => None,
        Needs::CargoTool(tool) => (!answers(process::cargo(root).args([tool, "--version"]))).then(|| format!("cargo install --locked cargo-{tool}")),
        Needs::Program(program) => (!answers(Command::new(program).arg("--version"))).then(|| format!("{program} (cargo install --locked {program}-cli)")),
        Needs::Toolchain(version) => (!answers(process::rustup_cargo(root).arg(format!("+{version}")).arg("--version")))
            .then(|| format!("rustup toolchain install {version} --profile minimal")),
        Needs::Target(triple, compilers) => {
            let installed =
                process::output(Command::new("rustup").args(["target", "list", "--installed"])).is_ok_and(|out| out.lines().any(|l| l.trim() == triple));
            let compiler = compilers.iter().any(|c| answers(Command::new(c).arg("--version")));
            match (installed, compiler) {
                (true, true) => None,
                (false, _) => Some(format!("rustup target add {triple}")),
                (true, false) => Some(format!("a MinGW-w64 C compiler on PATH ({})", compilers.join(" or "))),
            }
        }
    }
}

/// `cargo <args>` in the checkout; whether it passed.
fn cargo(root: &Path, args: &[&str]) -> Res<bool> {
    process::passed(process::cargo(root).args(args))
}

/// The documentation, a warning an error.
fn doc(root: &Path) -> Res<bool> {
    process::passed(
        process::cargo(root)
            .env("RUSTDOCFLAGS", "-D warnings")
            .args(["doc", "--workspace", "--no-deps"]),
    )
}

/// Every crate checked with the minimum toolchain.
fn msrv(root: &Path) -> Res<bool> {
    process::passed(
        process::rustup_cargo(root)
            .arg(format!("+{MSRV}"))
            .args(["check", "--workspace", "--all-targets"]),
    )
}

/// The semver check CI runs, from its own workflow (its excludes follow the tree).
fn semver(root: &Path) -> Res<bool> {
    let workflow = std::fs::read_to_string(root.join(".github/workflows/ci.yml"))?;
    let args = semver_args(&workflow).ok_or("ci.yml runs no `cargo semver-checks`")?;
    process::passed(process::cargo(root).args(&args))
}

/// The arguments of the `cargo semver-checks` line `workflow` runs, the workflow's own variables
/// (`$RELEASE_ARGS`) left out.
fn semver_args(workflow: &str) -> Option<Vec<String>> {
    let line = workflow.lines().map(str::trim).find(|l| l.starts_with("cargo semver-checks"))?;
    Some(line.split_whitespace().skip(1).filter(|w| !w.starts_with('$')).map(str::to_owned).collect())
}

/// Isothrum runs what Isoloom writes and links none of it: `Cargo.lock` lists no Isoloom crate,
/// and no `.cargo/config.toml` patches one in.
fn isoloom_free(root: &Path) -> Res<bool> {
    let lock = std::fs::read_to_string(root.join("Cargo.lock"))?;
    let config = std::fs::read_to_string(root.join(".cargo/config.toml")).unwrap_or_default();
    let found = isoloom_found(&lock, &config);
    for why in &found {
        eprintln!("check isoloom-free: {why}");
    }
    Ok(found.is_empty())
}

/// What ties the workspace to Isoloom, read from its lockfile `lock` and its cargo configuration
/// `config`.
fn isoloom_found(lock: &str, config: &str) -> Vec<String> {
    let mut found: Vec<String> = lock
        .lines()
        .filter_map(|l| l.strip_prefix("name = \""))
        .filter_map(|n| n.strip_suffix('"'))
        .filter(|n| n.starts_with("isoloom"))
        .map(|n| format!("Cargo.lock lists the Isoloom crate `{n}`"))
        .collect();
    if config.lines().any(|l| l.trim_start().starts_with("[patch")) {
        found.push(".cargo/config.toml patches crates (an Isoloom checkout): Isothrum links none".to_owned());
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_short_list_is_fetch_fmt_clippy_the_tests_and_windows_clippy_and_every_step_has_a_name_of_its_own() {
        let fast: Vec<&str> = STEPS.iter().filter(|s| s.fast).map(|s| s.name).collect();
        assert_eq!(fast, ["fetch", "fmt", "clippy", "test", "windows-gnu"]);
        let all: Vec<&str> = STEPS.iter().map(|s| s.name).collect();
        for name in ["doc", "quality", "regenerate", "typos", "machete", "deny", "msrv", "semver", "isoloom-free"] {
            assert!(all.contains(&name), "{name} in the full check");
        }
        let mut unique = all.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), all.len(), "{all:?}");
    }

    #[test]
    fn the_summary_says_each_step_and_any_red_or_missing_one_makes_the_check_red() {
        let (lines, green) = summary(&[("fmt", Outcome::Ok), ("clippy", Outcome::Ok)]);
        assert_eq!(
            (lines, green),
            (
                vec!["check: fmt: ok".to_owned(), "check: clippy: ok".to_owned(), "check: green".to_owned()],
                true
            )
        );
        let (said, passed) = summary(&[
            ("fmt", Outcome::Red),
            ("deny", Outcome::Missing("cargo install --locked cargo-deny".into())),
            ("doc", Outcome::Ok),
        ]);
        assert!(!passed);
        assert_eq!(
            said,
            [
                "check: fmt: red",
                "check: deny: red (not installed: cargo install --locked cargo-deny)",
                "check: doc: ok",
                "check: red"
            ]
        );
        assert!(!summary(&[("deny", Outcome::Missing("x".into()))]).1, "a step that didn't run never passes");
    }

    #[test]
    fn the_semver_step_runs_the_line_ci_runs_without_its_variables() {
        let workflow = "      - name: cargo semver-checks\n        run: |\n          cargo semver-checks --workspace --exclude xtask $RELEASE_ARGS\n";
        assert_eq!(
            semver_args(workflow),
            Some(vec![
                "semver-checks".to_owned(),
                "--workspace".to_owned(),
                "--exclude".to_owned(),
                "xtask".to_owned()
            ])
        );
        assert_eq!(semver_args("jobs: {}\n"), None);
        let ours = std::fs::read_to_string(repo::root().join(".github/workflows/ci.yml")).expect("ci.yml");
        assert!(
            semver_args(&ours).is_some_and(|a| a.contains(&"--workspace".to_owned())),
            "this repository's CI runs it"
        );
    }

    #[test]
    fn an_isoloom_crate_in_the_lockfile_or_a_patch_in_the_cargo_config_is_found() {
        let clean = "[[package]]\nname = \"isothrum-runner\"\n";
        assert_eq!(isoloom_found(clean, "[alias]\nxtask = \"run -p xtask --\"\n"), Vec::<String>::new());
        let linked = "[[package]]\nname = \"isoloom-spec\"\n";
        assert_eq!(isoloom_found(linked, ""), ["Cargo.lock lists the Isoloom crate `isoloom-spec`"]);
        assert_eq!(isoloom_found(clean, "[patch.crates-io]\nisoloom-spec = { path = \"../isoloom\" }\n").len(), 1);
        let ours = std::fs::read_to_string(repo::root().join("Cargo.lock")).expect("Cargo.lock");
        assert_eq!(isoloom_found(&ours, ""), Vec::<String>::new(), "this workspace links no Isoloom crate");
    }
}
