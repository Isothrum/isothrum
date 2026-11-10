//! `msrv` and `features`, through cargo-hack (`cargo install --locked cargo-hack`):
//!
//! - `msrv`: every crate checked with the toolchain its `rust-version` names (`cargo hack check
//!   --rust-version`), so the declared minimum is a fact, not a hope. rustup must have that
//!   toolchain (`rustup toolchain install <version> --profile minimal`).
//! - `features`: every combination of every crate's features checked (`cargo hack check
//!   --feature-powerset`), so features stay additive and no combination breaks the build.
//!
//! A checkout patched to a local Isoloom (a `[patch.crates-io]` in a git-ignored
//! `.cargo/config.toml`, or the workspace repository's) resolves the patched crates into the
//! committed `Cargo.lock`, and the minimum toolchain's older Cargo may write it in another format.
//! `msrv` puts the committed lockfile back afterwards, so the check never leaves it changed.

use std::path::Path;
use std::process::ExitCode;

use crate::{Res, process, repo};

/// Checks every crate with the toolchain of its `rust-version`.
pub(crate) fn msrv() -> Res<ExitCode> {
    let root = repo::root();
    let lock = root.join("Cargo.lock");
    let committed = std::fs::read(&lock)?;
    let checked = hack(&root, &["check", "--rust-version", "--workspace"]);
    if std::fs::read(&lock)? != committed {
        std::fs::write(&lock, committed)?;
        println!("msrv: Cargo.lock put back as committed (the minimum toolchain's Cargo rewrote it)");
    }
    checked
}

/// Checks every crate with every combination of its features.
pub(crate) fn features() -> Res<ExitCode> {
    hack(&repo::root(), &["check", "--feature-powerset", "--workspace"])
}

fn hack(root: &Path, args: &[&str]) -> Res<ExitCode> {
    process::require_cargo_tool(root, "hack")?;
    // The toolchain `--rust-version` asks for (`cargo +<version>`) must win over the one rustup
    // picked for this xtask.
    let mut cmd = process::rustup_cargo(root);
    cmd.arg("hack").args(args);
    Ok(process::exit_code(process::passed(&mut cmd)?))
}
