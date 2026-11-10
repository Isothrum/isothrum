//! `quality`: the ratchet of the clean-code rules (lints, file size, duplication, unused
//! dependencies, exceptions). Every count of a rule, per crate, against `quality-baseline.json`: a
//! count that goes up fails; one that goes down is written down with `--update`, which never
//! raises one.
//!
//! What is counted, per crate (the fuzz crate, `fuzz/`, a workspace of its own, included):
//! - every warning `cargo clippy --workspace --all-targets` gives (`clippy/<lint>`,
//!   `rustc/<lint>`): a lint the workspace's `[lints]` table would hold at warn. Every one is at
//!   deny today, and fails the build, so this counts none; it is there for a lint that can't be
//!   at deny yet. (The fuzz crate's lints, also at deny, are `xtask fuzz --check`'s.)
//! - source files over 400 lines of code (`file/over-400-loc`), unit tests' files and fuzz
//!   targets included;
//! - blocks of code found twice (`dup/clone`, jscpd over the Rust sources);
//! - dependencies nothing uses (`machete/unused`) and the ones machete is told to ignore
//!   (`machete/ignored`);
//! - the exceptions, `#[expect(lint, reason = "…")]` (`exception/expect`): each is reviewed, so a
//!   new one raises a count too.

mod baseline;
mod clippy;
mod dup;
mod machete;
mod sources;

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::{Res, repo};

/// Each rule's count per crate (the crates with none left out).
pub(crate) type Counts = BTreeMap<String, BTreeMap<String, u64>>;

/// The arguments of `quality`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Write the counts into the baseline (only when none went up).
    #[arg(long)]
    update: bool,
    /// Print every count, rule by rule and crate by crate.
    #[arg(long)]
    table: bool,
}

/// A crate of the workspace: its name and folder, relative to the root with `/`.
#[derive(Debug, Clone)]
pub(crate) struct Crate {
    /// Its package name.
    pub(crate) name: String,
    /// Its folder, relative to the workspace root.
    pub(crate) dir: String,
}

/// The crate holding `file` (relative to the root, with `/`): the one with the longest folder
/// that contains it, else `workspace`.
pub(crate) fn crate_of(crates: &[Crate], file: &str) -> String {
    crates
        .iter()
        .filter(|c| file.starts_with(&format!("{}/", c.dir)))
        .max_by_key(|c| c.dir.len())
        .map_or_else(|| "workspace".to_owned(), |c| c.name.clone())
}

/// Adds one finding of `rule` in `krate`.
pub(crate) fn add(counts: &mut Counts, rule: &str, krate: &str) {
    *counts.entry(rule.to_owned()).or_default().entry(krate.to_owned()).or_default() += 1;
}

/// Measures, then checks against (or with `--update`, rewrites) the baseline.
pub(crate) fn run(a: &Args) -> Res<ExitCode> {
    let root = repo::root();
    let crates = sources::crates(&root)?;
    let mut counts = Counts::new();
    clippy::measure(&root, &crates, &mut counts)?;
    sources::measure(&root, &crates, &mut counts)?;
    dup::measure(&root, &crates, &mut counts)?;
    machete::measure(&root, &crates, &mut counts)?;
    let file = root.join("quality-baseline.json");
    let Some(before) = baseline::read(&file)? else {
        // The first measure is the baseline.
        print!("{}", baseline::table(&counts));
        baseline::write(&file, &counts)?;
        println!("quality: no baseline yet; {} written", file.display());
        return Ok(ExitCode::SUCCESS);
    };
    let changes = baseline::compare(&before, &counts);
    if a.table || a.update {
        print!("{}", baseline::table(&counts));
    }
    for line in &changes.lines {
        println!("{line}");
    }
    if !changes.raised.is_empty() {
        println!("quality: {} count(s) went up; bring them back down (see above)", changes.raised.len());
        return Ok(ExitCode::FAILURE);
    }
    if a.update {
        baseline::write(&file, &counts)?;
        println!("quality: {} written", file.display());
    } else if !changes.lowered.is_empty() {
        println!(
            "quality: {} count(s) went down: `cargo run -p xtask -- quality --update` writes them",
            changes.lowered.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_counts_in_the_innermost_crate_holding_it() {
        let crates = [
            Crate {
                name: "outer".into(),
                dir: "crates".into(),
            },
            Crate {
                name: "isothrum-runner".into(),
                dir: "crates/isothrum-runner".into(),
            },
        ];
        assert_eq!(crate_of(&crates, "crates/isothrum-runner/src/lib.rs"), "isothrum-runner");
        assert_eq!(crate_of(&crates, "crates/other/src/lib.rs"), "outer");
        assert_eq!(crate_of(&crates, "fuzz/x.rs"), "workspace");
    }
}
