//! `fuzz`: the fuzz targets of `fuzz/` (cargo-fuzz), each run for a while with nightly
//! and libFuzzer, from its own corpus (`fuzz/corpus/<target>`, which grows) and the seeds
//! `fuzz/Cargo.toml` names (`[package.metadata.seeds]`: the fixtures).
//!
//! A target that crashes (a panic, a failed assertion, an input that takes over 10 seconds) fails
//! the run; libFuzzer leaves the input under `fuzz/artifacts/<target>/`. Each crash becomes a
//! regression test in the crate it belongs to, then the fix. Isoloom's `cargo xtask check-all
//! --deep` runs every target for the default time.
//!
//! `--check` builds nothing with nightly: it checks the fuzz crate's format and lints on the
//! pinned toolchain instead, so a change to the APIs it calls shows in `check-all`.
//!
//! The fuzz crate is a workspace of its own, and Cargo runs in its folder, so it reads the
//! configuration there: a checkout Isoloom's `dev-link` links has one in `fuzz/.cargo/` too, with
//! a lockfile of its own. One whose root is linked and whose fuzz crate isn't is refused, since
//! Cargo would resolve the fuzz crate into the root's linked lockfile.
//!
//! Needs `cargo install --locked cargo-fuzz` and `rustup toolchain install nightly --profile
//! minimal`. The builds go to `<target dir>/fuzz`.

use std::path::Path;
use std::process::ExitCode;

use crate::{Res, process, repo};

/// The first line of the configuration Isoloom's `dev-link` writes.
const DEV_LINK_MARKER: &str = "# Written by isoloom `cargo xtask dev-link`.";

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
    refuse_half_link(&root, &fuzz)?;
    if a.check {
        return check(&root, &fuzz);
    }
    let mut targets = targets(&std::fs::read_to_string(fuzz.join("Cargo.toml"))?)?;
    if !a.targets.is_empty() {
        if let Some(unknown) = a.targets.iter().find(|t| !targets.iter().any(|known| known.name == **t)) {
            return Err(format!("no fuzz target {unknown} in fuzz/Cargo.toml").into());
        }
        targets.retain(|t| a.targets.contains(&t.name));
    }
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

/// Fails when the root is dev-linked and the fuzz crate isn't (see the module's docs).
fn refuse_half_link(root: &Path, fuzz: &Path) -> Res<()> {
    let linked = |dir: &Path| std::fs::read_to_string(dir.join(".cargo").join("config.toml")).is_ok_and(|t| t.lines().any(|l| l == DEV_LINK_MARKER));
    if linked(root) && !linked(fuzz) {
        return Err("this checkout is dev-linked but fuzz/ isn't: run Isoloom's `cargo xtask dev-link link` for it again".into());
    }
    Ok(())
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

/// Checks the fuzz crate's format and lints on the pinned toolchain.
fn check(root: &Path, fuzz: &Path) -> Res<ExitCode> {
    let formatted = process::passed(process::cargo(fuzz).args(["fmt", "--check"]))?;
    let mut clippy = process::cargo(fuzz);
    clippy.args(["clippy", "--all-targets", "--target-dir"]).arg(repo::target_dir(root));
    clippy.args(["--", "-D", "warnings"]);
    Ok(process::exit_code(process::passed(&mut clippy)? && formatted))
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
    fn a_linked_root_with_an_unlinked_fuzz_crate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (root, fuzz) = (dir.path(), dir.path().join("fuzz"));
        std::fs::create_dir_all(root.join(".cargo")).unwrap();
        std::fs::create_dir_all(fuzz.join(".cargo")).unwrap();
        assert!(refuse_half_link(root, &fuzz).is_ok(), "nothing linked");
        std::fs::write(root.join(".cargo/config.toml"), format!("{DEV_LINK_MARKER}\n")).unwrap();
        assert!(refuse_half_link(root, &fuzz).is_err());
        std::fs::write(fuzz.join(".cargo/config.toml"), format!("{DEV_LINK_MARKER}\n")).unwrap();
        assert!(refuse_half_link(root, &fuzz).is_ok(), "both linked");
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
