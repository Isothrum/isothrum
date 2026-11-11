//! `fuzz`: the fuzz targets of `fuzz/` (cargo-fuzz), each run for a while with nightly
//! and libFuzzer, from its own corpus (`fuzz/corpus/<target>`, which grows) and the seeds
//! `fuzz/Cargo.toml` names (`[package.metadata.seeds]`: the fixtures).
//!
//! A target that crashes (a panic, a failed assertion, an input that takes over 10 seconds) fails
//! the run; libFuzzer leaves the input under `fuzz/artifacts/<target>/`. Each crash becomes a
//! regression test in the crate it belongs to, then the fix. `cargo run -p xtask -- fuzz` runs
//! every target for the default time.
//!
//! `--check` builds nothing with nightly: it checks the fuzz crate on the pinned toolchain
//! instead (its format, its lints, and `cargo deny` over its own lockfile), so a change to the APIs
//! its targets call, or an advisory against a crate only it locks, fails a gate. CI and
//! CONTRIBUTING's gate list run it.
//!
//! The fuzz crate is a workspace of its own, and Cargo runs in its folder: it reads the
//! `.cargo/config.toml` of that folder and of each one above it, so a `[patch.crates-io]` at the
//! root (or one in a folder above the checkout) applies to it too.
//!
//! Needs `cargo install --locked cargo-fuzz` and `rustup toolchain install nightly --profile
//! minimal`. The builds go to `<target dir>/fuzz`.

use std::path::Path;
use std::process::ExitCode;

use crate::{Res, process, repo};

/// The arguments of `fuzz`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// How long each target runs, in seconds.
    #[arg(long, value_name = "SECONDS", default_value_t = 60)]
    time: u32,
    /// Check the fuzz crate's format and lints on the pinned toolchain instead of running it.
    #[arg(long)]
    check: bool,
    /// The targets (default: every one in fuzz/Cargo.toml).
    targets: Vec<String>,
}

/// One fuzz target: its name and the folders of inputs it starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    name: String,
    seeds: Vec<String>,
}

/// Runs the targets one after the other (or, with `--check`, checks the crate).
pub(crate) fn run(a: &Args) -> Res<ExitCode> {
    let root = repo::root();
    let fuzz = root.join("fuzz");
    if a.check {
        return check(&root, &fuzz);
    }
    let targets = chosen(targets(&std::fs::read_to_string(fuzz.join("Cargo.toml"))?)?, &a.targets)?;
    let mut failed = Vec::new();
    for t in &targets {
        if !fuzz_one(&root, t, a.time)? {
            failed.push(&t.name);
        }
    }
    for name in &failed {
        println!("fuzz: {name} crashed; the input is under fuzz/artifacts/{name}/: make it a regression test, then fix it");
    }
    println!("fuzz: {} of {} target(s) clean, {} s each", targets.len() - failed.len(), targets.len(), a.time);
    Ok(process::exit_code(failed.is_empty()))
}

/// The targets `known` that `asked` names, or every one when it names none; refused when it names
/// one `fuzz/Cargo.toml` doesn't declare.
fn chosen(mut known: Vec<Target>, asked: &[String]) -> Res<Vec<Target>> {
    if !asked.is_empty() {
        if let Some(unknown) = asked.iter().find(|t| !known.iter().any(|k| k.name == **t)) {
            return Err(format!("no fuzz target {unknown} in fuzz/Cargo.toml").into());
        }
        known.retain(|t| asked.contains(&t.name));
    }
    Ok(known)
}

/// Runs one target for `secs` seconds; whether it found nothing.
fn fuzz_one(root: &Path, t: &Target, secs: u32) -> Res<bool> {
    let fuzz = root.join("fuzz");
    let corpus = fuzz.join("corpus").join(&t.name);
    std::fs::create_dir_all(&corpus)?;
    println!("fuzz: {} for {secs} s", t.name);
    // The rustup proxy, so `+nightly` picks the toolchain; libFuzzer needs nightly's sanitizers.
    let mut cmd = process::rustup_cargo(&fuzz);
    cmd.args(["+nightly", "fuzz", "run", "--fuzz-dir", ".", "--debug-assertions", "--target-dir"])
        .arg(repo::target_dir(root).join("fuzz"));
    cmd.arg(&t.name).arg(&corpus);
    cmd.args(t.seeds.iter().map(|s| fuzz.join(s)));
    cmd.args(["--", &format!("-max_total_time={secs}"), "-timeout=10", "-print_final_stats=1"]);
    process::passed(&mut cmd).map_err(|e| format!("{e} (cargo install --locked cargo-fuzz; rustup toolchain install nightly)").into())
}

/// Checks the fuzz crate's format, lints and supply chain on the pinned toolchain.
fn check(root: &Path, fuzz: &Path) -> Res<ExitCode> {
    let formatted = process::passed(process::cargo(fuzz).args(["fmt", "--check"]))?;
    let mut clippy = process::cargo(fuzz);
    clippy.args(["clippy", "--all-targets", "--target-dir"]).arg(repo::target_dir(root));
    clippy.args(["--", "-D", "warnings"]);
    let linted = process::passed(&mut clippy)?;
    // The fuzz crate's own lockfile, which the root's check doesn't see, held to the root's
    // deny.toml (cargo-deny looks for it in the folders above too).
    let mut deny = process::cargo(fuzz);
    deny.args(["deny", "check"]);
    let supplied = process::passed(&mut deny)?;
    Ok(process::exit_code(formatted && linted && supplied))
}

/// The targets `fuzz/Cargo.toml` declares (its `[[bin]]`s), with their seed folders.
fn targets(manifest: &str) -> Res<Vec<Target>> {
    let doc: toml::Table = manifest.parse()?;
    let seeds = doc
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("seeds"))
        .and_then(toml::Value::as_table);
    let bins = doc.get("bin").and_then(toml::Value::as_array).ok_or("fuzz/Cargo.toml declares no [[bin]]")?;
    Ok(bins
        .iter()
        .filter_map(|b| b.get("name").and_then(toml::Value::as_str))
        .map(|name| Target {
            name: name.to_owned(),
            seeds: seeds
                .and_then(|s| s.get(name))
                .and_then(toml::Value::as_array)
                .map(|list| list.iter().filter_map(toml::Value::as_str).map(str::to_owned).collect())
                .unwrap_or_default(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_bin_is_a_target_with_the_seeds_named_for_it() {
        let manifest = "[package]\nname = \"f\"\n[package.metadata.seeds]\na = [\"../fixtures\"]\n\n[[bin]]\nname = \"a\"\npath = \"a.rs\"\n\n[[bin]]\nname = \"b\"\npath = \"b.rs\"\n";
        let found = targets(manifest).unwrap();
        assert_eq!(
            found,
            [
                Target {
                    name: "a".into(),
                    seeds: vec!["../fixtures".into()]
                },
                Target {
                    name: "b".into(),
                    seeds: Vec::new()
                }
            ]
        );
    }

    #[test]
    fn the_targets_asked_for_are_run_and_an_unknown_one_is_refused() {
        let known = || {
            ["a", "b"]
                .map(|name| Target {
                    name: name.into(),
                    seeds: Vec::new(),
                })
                .to_vec()
        };
        assert_eq!(chosen(known(), &[]).unwrap(), known(), "every one by default");
        assert_eq!(
            chosen(known(), &["b".into()]).unwrap(),
            [Target {
                name: "b".into(),
                seeds: Vec::new()
            }]
        );
        let refused = chosen(known(), &["b".into(), "c".into()]).unwrap_err().to_string();
        assert_eq!(refused, "no fuzz target c in fuzz/Cargo.toml");
    }

    #[test]
    fn the_fuzz_crate_declares_every_target_with_existing_seeds() {
        let fuzz = repo::root().join("fuzz");
        let found = targets(&std::fs::read_to_string(fuzz.join("Cargo.toml")).unwrap()).unwrap();
        assert!(found.iter().any(|t| t.name == "drawing"), "{found:?}");
        for t in &found {
            assert!(fuzz.join("fuzz_targets").join(format!("{}.rs", t.name)).is_file(), "{}", t.name);
            for seed in &t.seeds {
                assert!(fuzz.join(seed).is_dir(), "{}: seed folder {seed} is missing", t.name);
            }
        }
    }
}
