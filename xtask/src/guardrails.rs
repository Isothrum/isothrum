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
//!   `*_tests.rs`, no `test_` prefix on a test's name);
//! - `vocabulary`: no file under `crates/` names Isoloom or uses an application's words (lab,
//!   CTF, player, attack box), a test's data included; and no document names Isoloom but the
//!   README's one line;
//! - `consent`: nothing accepts a licence, an agreement or terms on the user's behalf (no
//!   `--accept-license`, no winget `--accept-*-agreements`, no EULA switch) in the crates' code;
//! - `flakes`: no test waits on the clock (a `sleep` there is counted, each with why it is no
//!   wait), none lets a free port go before it uses it, and a stand-in written by each test
//!   is counted;
//! - `licences`: the release accepts the licences deny.toml allows, no fewer.
//!
//! Starting a process outside an adapter, and `unwrap()` outside tests, are clippy's
//! (`disallowed-methods` in clippy.toml, `unwrap_used` in the workspace's lints).

mod boundary;
mod capabilities;
mod consent;
mod flakes;
mod layout;
mod licences;
mod scans;
mod source;
mod vocabulary;
