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
    /// Prints the steps and exits.
    #[arg(long)]
    list: bool,
    /// Only these steps (comma-separated names, as `--list` prints them).
    #[arg(long, value_delimiter = ',', value_name = "STEPS")]
    only: Vec<String>,
    /// Every step but these (comma-separated names).
    #[arg(long, value_delimiter = ',', value_name = "STEPS")]
    skip: Vec<String>,
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
    /// It runs on another OS only (`Step::only_on`): said, neither a pass nor a failure.
    NotHere(&'static str),
}

/// What a step needs installed beyond cargo, and how to get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Needs {
    /// Nothing more.
    Nothing,
    /// A cargo subcommand (`cargo <name> --version` answers).
    CargoTool(&'static str),
    /// A program on `PATH` (`<name> --version` answers), and how to install it.
    Program(&'static str, &'static str),
    /// A rustup toolchain.
    Toolchain(&'static str),
    /// A rustup target, and a C compiler for it on `PATH` (the bundled SQLite's build script).
    Target(&'static str, &'static [&'static str]),
}

/// One step: its name, whether the short list has it, what it needs, what it runs, and the one
/// OS it runs on when it needs one (`std::env::consts::OS`).
struct Step {
    name: &'static str,
    fast: bool,
    needs: Needs,
    run: fn(&Path) -> Res<bool>,
    only_on: Option<&'static str>,
}

/// The Windows target the Windows clippy checks, and the C compilers it may build SQLite with.
const WINDOWS: (&str, &[&str]) = ("x86_64-pc-windows-gnu", &["x86_64-w64-mingw32-gcc", "x86_64-w64-mingw32-clang"]);

/// The minimum toolchain, the workspace's `rust-version`.
const MSRV: &str = "1.90";

/// Every step, in the order they run.
const STEPS: [Step; 17] = [
    Step {
        name: "fetch",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["fetch"]),
        only_on: None,
    },
    Step {
        name: "isoloom-free",
        fast: false,
        needs: Needs::Nothing,
        run: isoloom_free,
        only_on: None,
    },
    Step {
        name: "fmt",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["fmt", "--all", "--check"]),
        only_on: None,
    },
    Step {
        name: "clippy",
        fast: true,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        only_on: None,
    },
    Step {
        name: "test",
        fast: true,
        needs: Needs::CargoTool("nextest"),
        run: |r| cargo(r, &["run", "-p", "xtask", "-q", "--", "test"]),
        only_on: None,
    },
    Step {
        name: "doc",
        fast: false,
        needs: Needs::Nothing,
        run: doc,
        only_on: None,
    },
    Step {
        name: "quality",
        fast: false,
        needs: Needs::Nothing,
        run: |r| cargo(r, &["run", "-p", "xtask", "-q", "--", "quality"]),
        only_on: None,
    },
    Step {
        name: "regenerate",
        fast: false,
        needs: Needs::Nothing,
        run: |r| process::passed(Command::new("sh").arg(".githooks/regenerate").arg("--check").current_dir(r)),
        only_on: None,
    },
    Step {
        name: "typos",
        fast: false,
        needs: Needs::Program("typos", "cargo install --locked typos-cli"),
        run: |r| process::passed(Command::new("typos").current_dir(r)),
        only_on: None,
    },
    Step {
        name: "machete",
        fast: false,
        needs: Needs::CargoTool("machete"),
        run: |r| cargo(r, &["machete"]),
        only_on: None,
    },
    Step {
        name: "deny",
        fast: false,
        needs: Needs::CargoTool("deny"),
        run: |r| cargo(r, &["deny", "--all-features", "check"]),
        only_on: None,
    },
    Step {
        name: "msrv",
        fast: false,
        needs: Needs::Toolchain(MSRV),
        run: msrv,
        only_on: None,
    },
    Step {
        name: "windows-gnu",
        fast: true,
        needs: Needs::Target(WINDOWS.0, WINDOWS.1),
        run: |r| cargo(r, &["clippy", "--workspace", "--all-targets", "--target", WINDOWS.0, "--", "-D", "warnings"]),
        only_on: None,
    },
    Step {
        name: "semver",
        fast: false,
        needs: Needs::CargoTool("semver-checks"),
        run: semver,
        only_on: None,
    },
    Step {
        name: "hooks",
        fast: false,
        // The hooks' test pushes through the pre-push hook, which refuses without gitleaks.
        needs: Needs::Program("gitleaks", "brew install gitleaks, or https://github.com/gitleaks/gitleaks#installing"),
        run: hooks,
        only_on: None,
    },
    Step {
        name: "fuzz",
        fast: false,
        needs: Needs::CargoTool("deny"),
        run: |r| cargo(r, &["run", "-p", "xtask", "-q", "--", "fuzz", "--check"]),
        only_on: None,
    },
    Step {
        name: "windows-binary",
        fast: false,
        needs: Needs::Nothing,
        run: windows_binary,
        only_on: Some("windows"),
    },
];

/// Runs the steps `a` asks for, each whatever the others did, then says how each ended.
pub(crate) fn run(a: &Args) -> ExitCode {
    let chosen = match chosen(a) {
        Ok(chosen) => chosen,
        Err(e) => {
            eprintln!("check: {e}");
            return ExitCode::from(2);
        }
    };
    if a.list {
        for s in &chosen {
            println!(
                "{:<15}{}{}",
                s.name,
                if s.fast { "(fast) " } else { "" },
                s.only_on.map(|os| format!("({os} only)")).unwrap_or_default()
            );
        }
        return ExitCode::SUCCESS;
    }
    let root = repo::root();
    let mut ended = Vec::new();
    for step in chosen {
        println!("=== check {}", step.name);
        if let Some(os) = step.only_on.filter(|os| *os != std::env::consts::OS) {
            ended.push((step.name, Outcome::NotHere(os)));
            continue;
        }
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

/// The steps `a` asks for, in order: the short list with `--fast`, then `--only` and `--skip` by
/// name. An error names a step that doesn't exist.
fn chosen(a: &Args) -> Res<Vec<&'static Step>> {
    for name in a.only.iter().chain(&a.skip) {
        if !STEPS.iter().any(|s| s.name == name) {
            let names: Vec<&str> = STEPS.iter().map(|s| s.name).collect();
            return Err(format!("no step `{name}`; the steps: {}", names.join(", ")).into());
        }
    }
    Ok(STEPS
        .iter()
        .filter(|s| !a.fast || s.fast)
        .filter(|s| a.only.is_empty() || a.only.iter().any(|n| n == s.name))
        .filter(|s| !a.skip.iter().any(|n| n == s.name))
        .collect())
}

/// One line per step (`fmt: ok`, `deny: red (missing: cargo install --locked cargo-deny)`), and
/// whether every step passed: a missing tool is red, a step for another OS neither.
fn summary(ended: &[(&str, Outcome)]) -> (Vec<String>, bool) {
    let mut lines: Vec<String> = ended
        .iter()
        .map(|(name, outcome)| match outcome {
            Outcome::Ok => format!("check: {name}: ok"),
            Outcome::Red => format!("check: {name}: red"),
            Outcome::Missing(install) => format!("check: {name}: red (not installed: {install})"),
            Outcome::NotHere(os) => format!("check: {name}: n/a ({os} only)"),
        })
        .collect();
    let green = ended.iter().all(|(_, o)| matches!(o, Outcome::Ok | Outcome::NotHere(_)));
    lines.push(format!("check: {}", if green { "green" } else { "red" }));
    (lines, green)
}

/// What to install for `needs`, when it isn't there.
fn missing(root: &Path, needs: Needs) -> Option<String> {
    let answers = |cmd: &mut Command| process::first_line(cmd).is_some();
    match needs {
        Needs::Nothing => None,
        Needs::CargoTool(tool) => (!answers(process::cargo(root).args([tool, "--version"]))).then(|| format!("cargo install --locked cargo-{tool}")),
        Needs::Program(program, install) => (!answers(Command::new(program).arg("--version"))).then(|| format!("{program} ({install})")),
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

/// The library API against each crate's latest release on crates.io (cargo-semver-checks), the
/// crates with no library API or no release yet left out. The release type comes from the
/// workspace version: when it is still the published one, the changes are checked as a patch, so
/// a breaking change fails until the version is raised on purpose. One exception, decided for the
/// release after 1.x: while crates.io's latest is a 1.x, the unreleased changes are checked as the
/// 2.0.0 major they are. Once 2.0.0 is published, the exception no longer matches.
fn semver(root: &Path) -> Res<bool> {
    let answer = process::output(Command::new("curl").args([
        "-fsS",
        "-A",
        "isothrum-xtask (https://github.com/Isothrum/isothrum)",
        "https://crates.io/api/v1/crates/isothrum-runner",
    ]))?;
    let published = latest_release(&answer)?;
    println!("latest isothrum-runner on crates.io: {published}");
    let mut args = vec!["semver-checks", "--workspace"];
    // isothrum-cli is a binary (no library API); xtask is not published; isothrum-client and
    // isothrum-service have no release on crates.io yet: no baseline.
    for crate_name in SEMVER_LEFT_OUT {
        args.extend(["--exclude", crate_name]);
    }
    args.extend(release_type(&published));
    process::passed(process::cargo(root).args(&args))
}

/// The crates `semver` leaves out: no library API, or no release to compare with yet.
const SEMVER_LEFT_OUT: [&str; 4] = ["isothrum-cli", "isothrum-client", "isothrum-service", "xtask"];

/// crates.io's latest stable version, from its crate answer.
fn latest_release(answer: &str) -> Res<String> {
    let v: serde_json::Value = serde_json::from_str(answer)?;
    v.get("crate")
        .and_then(|c| c.get("max_stable_version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("no max_stable_version in crates.io's answer: {answer}").into())
}

/// `--release-type major` while crates.io's latest is a 1.x, nothing after.
fn release_type(published: &str) -> Vec<&'static str> {
    if published.starts_with("1.") {
        vec!["--release-type", "major"]
    } else {
        Vec::new()
    }
}

/// shellcheck over every hook and the set-up script, then the hooks' own test (gitleaks on PATH,
/// as the pre-push runs it).
fn hooks(root: &Path) -> Res<bool> {
    let mut scripts: Vec<String> = std::fs::read_dir(root.join(".githooks"))?
        .flatten()
        .filter(|e| e.path().is_file())
        .map(|e| format!(".githooks/{}", e.file_name().to_string_lossy()))
        .collect();
    scripts.sort();
    scripts.push("scripts/setup-hooks.sh".to_owned());
    let shellcheck = process::passed(Command::new("shellcheck").arg("-x").args(&scripts).current_dir(root))?;
    Ok(shellcheck && process::passed(Command::new("bash").arg(".githooks/test.sh").current_dir(root))?)
}

/// The release's Windows executable (the C runtime linked in, as release.yml builds it), then its
/// imports read with the toolchain's `llvm-objdump`: none of the Visual C++ runtime's DLLs, so a
/// Windows without the redistributable starts it (GitHub's runners have it: only the imports tell).
fn windows_binary(root: &Path) -> Res<bool> {
    let built = process::passed(
        process::cargo(root)
            .args(["build", "--release", "--locked", "-p", "isothrum-cli", "--target", "x86_64-pc-windows-msvc"])
            .env("CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS", "-C target-feature=+crt-static"),
    )?;
    if !built {
        return Ok(false);
    }
    let sysroot = process::output(Command::new("rustc").args(["--print", "sysroot"]).current_dir(root))?;
    let objdump = Path::new(sysroot.trim()).join("lib/rustlib/x86_64-pc-windows-msvc/bin/llvm-objdump.exe");
    let target = std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), std::path::PathBuf::from);
    let exe = target.join("x86_64-pc-windows-msvc/release/isothrum.exe");
    let listing = process::output(Command::new(&objdump).arg("-p").arg(&exe))?;
    let runtime = vc_runtime_imports(&listing);
    if !runtime.is_empty() {
        eprintln!(
            "{} imports {}: a Windows without the Visual C++ Redistributable can't start it (link the C runtime in: -C target-feature=+crt-static)",
            exe.display(),
            runtime.join(", ")
        );
    }
    Ok(runtime.is_empty())
}

/// The Visual C++ runtime's DLLs among the imports `llvm-objdump -p` lists (`DLL Name: <dll>`).
fn vc_runtime_imports(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.trim().strip_prefix("DLL Name:"))
        .map(|d| d.trim().to_owned())
        .filter(|d| {
            let lower = d.to_ascii_lowercase();
            (lower.starts_with("vcruntime") || lower.starts_with("msvcp")) && Path::new(&lower).extension().is_some_and(|e| e.eq_ignore_ascii_case("dll"))
        })
        .collect()
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
    // The linked packages only (`[[package]]`): a patch the build had no use for is listed under
    // `[[patch.unused]]` and links nothing.
    let mut section = "";
    let mut found: Vec<String> = lock
        .lines()
        .filter(|l| {
            if l.starts_with('[') {
                section = l.trim();
            }
            section == "[[package]]"
        })
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
        for name in [
            "doc",
            "quality",
            "regenerate",
            "typos",
            "machete",
            "deny",
            "msrv",
            "semver",
            "isoloom-free",
            "hooks",
            "fuzz",
            "windows-binary",
        ] {
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
    fn the_semver_step_checks_a_1x_release_as_a_major_and_a_later_one_as_the_version_says() {
        let answer = r#"{"crate":{"name":"isothrum-runner","max_stable_version":"1.4.2"}}"#;
        assert_eq!(latest_release(answer).ok().as_deref(), Some("1.4.2"));
        assert!(latest_release(r#"{"errors":[]}"#).is_err());
        assert_eq!(release_type("1.4.2"), ["--release-type", "major"]);
        assert_eq!(release_type("2.0.0"), Vec::<&str>::new());
        assert!(release_type("10.0.0").is_empty(), "a 10.x isn't a 1.x");
    }

    #[test]
    fn steps_are_chosen_by_name_and_an_unknown_one_is_named() {
        let args = |only: &[&str], skip: &[&str], fast: bool| Args {
            fast,
            list: false,
            only: only.iter().map(|s| (*s).to_owned()).collect(),
            skip: skip.iter().map(|s| (*s).to_owned()).collect(),
        };
        let names = |a: &Args| chosen(a).unwrap().iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(&args(&["fmt", "deny"], &[], false)), ["fmt", "deny"]);
        assert_eq!(names(&args(&[], &["windows-gnu"], true)), ["fetch", "fmt", "clippy", "test"]);
        assert!(
            chosen(&args(&["nope"], &[], false))
                .err()
                .is_some_and(|e| e.to_string().contains("no step `nope`"))
        );
    }

    #[test]
    fn a_step_for_another_os_is_said_and_neither_passes_nor_fails() {
        let (lines, green) = summary(&[("fmt", Outcome::Ok), ("windows-binary", Outcome::NotHere("windows"))]);
        assert!(green);
        assert_eq!(lines[1], "check: windows-binary: n/a (windows only)");
    }

    #[test]
    fn a_windows_binary_importing_the_visual_cpp_runtime_is_named() {
        let listing = "  DLL Name: KERNEL32.dll\n  DLL Name: VCRUNTIME140.dll\n  DLL Name: msvcp140.dll\n";
        assert_eq!(vc_runtime_imports(listing), ["VCRUNTIME140.dll", "msvcp140.dll"]);
        assert_eq!(vc_runtime_imports("  DLL Name: KERNEL32.dll\n"), Vec::<String>::new());
    }

    #[test]
    fn an_isoloom_crate_in_the_lockfile_or_a_patch_in_the_cargo_config_is_found() {
        let clean = "[[package]]\nname = \"isothrum-runner\"\n";
        assert_eq!(isoloom_found(clean, "[alias]\nxtask = \"run -p xtask --\"\n"), Vec::<String>::new());
        let linked = "[[package]]\nname = \"isoloom-spec\"\n";
        assert_eq!(isoloom_found(linked, ""), ["Cargo.lock lists the Isoloom crate `isoloom-spec`"]);
        let unused = "[[package]]\nname = \"isothrum-runner\"\n\n[[patch.unused]]\nname = \"isoloom-spec\"\nversion = \"3.0.0\"\n";
        assert_eq!(isoloom_found(unused, ""), Vec::<String>::new(), "an unused patch links nothing");
        assert_eq!(isoloom_found(clean, "[patch.crates-io]\nisoloom-spec = { path = \"../isoloom\" }\n").len(), 1);
        let ours = std::fs::read_to_string(repo::root().join("Cargo.lock")).expect("Cargo.lock");
        assert_eq!(isoloom_found(&ours, ""), Vec::<String>::new(), "this workspace links no Isoloom crate");
    }
}
