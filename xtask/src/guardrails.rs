//! The guardrails: tests that turn a class of bug into a failing build, read from what
//! `Cargo.lock` and the sources say.
//!
//! - `boundary`: the products stay apart, no Isothrum crate depends on an Isoloom crate, by the
//!   resolved graph `Cargo.lock` holds;
//! - `scans`: where the runner may parse a tool's output, and what a shell script it builds may
//!   hold (no `sh -c` assembled with `format!`, no `&&` chain in a script that runs as root);
//! - `capabilities`: every capability a driver claims (each action it takes, each hook it
//!   overrides) names the test that shows it reach its tool, or says why it reaches none;
//! - `layout`: tests live where CONTRIBUTING's "Tests" section says (no `#[path]`, no
//!   `*_tests.rs`, no `test_` prefix on a test's name).
//!
//! Starting a process outside an adapter, and `unwrap()` outside tests, are clippy's
//! (`disallowed-methods` in clippy.toml, `unwrap_used` in the workspace's lints).

mod boundary;
mod capabilities;
mod layout;
mod scans;
mod source;
