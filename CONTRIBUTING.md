# Contributing to Isothrum

Thank you for helping. Bug reports, fixes, documentation and new features are welcome. For a
large change, open an issue first so we can agree on the approach. Isothrum stays generic: it runs
any run manifest Isoloom writes, and holds nothing specific to one product built on it.

By contributing, you agree that your contribution is licensed under the Apache License 2.0, as the
project is. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Set up

1. Install Rust with [rustup](https://rustup.rs/). The toolchain is pinned by
   `rust-toolchain.toml` (1.99.0, with rustfmt and clippy); rustup installs it on first use.
2. Clone the repository and build:

   ```sh
   git clone https://github.com/Isothrum/isothrum
   cd isothrum
   cargo build
   ```

3. **When the code needs an unreleased Isoloom.** The Isoloom crates come from crates.io at the
   version `Cargo.toml` names. When a change needs Isoloom's next, unreleased version, clone
   Isoloom beside this repository and patch the crates to it in a git-ignored
   `.cargo/config.toml`, never committed:

   ```toml
   [patch.crates-io]
   isoloom-manifest = { path = "../isoloom/crates/core/isoloom-manifest" }
   isoloom-plan = { path = "../isoloom/crates/core/isoloom-plan" }
   isoloom-spec = { path = "../isoloom/crates/core/isoloom-spec" }
   isoloom-targets = { path = "../isoloom/crates/targets/isoloom-targets" }
   # ... and the Isoloom crates those depend on
   ```

   A patched build rewrites `Cargo.lock` and `fuzz/Cargo.lock` for the local crates: don't commit
   those changes.

Optional tools the gates use: `cargo install cargo-nextest cargo-machete cargo-deny
cargo-semver-checks typos-cli`, Node (the ratchet runs jscpd through `npx`), and `wasm-pack` for
the WebAssembly build.

## Gates

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs these on every pull request; run
them before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace                 # or: cargo run -p xtask -- test (nextest, then doctests)
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo run -p xtask -- quality          # the clean-code ratchet (quality-baseline.json)
cargo deny check
cargo machete
typos
cargo run -p xtask -- fuzz --check     # the fuzz crate (fuzz/, a workspace of its own): format, lints, cargo-deny
cargo semver-checks --workspace --exclude isothrum-cli --exclude xtask
```

Every lint in the workspace's `[lints]` is at deny: a finding fails the build. An exception is a
local `#[expect(lint, reason = "...")]`, never an `#[allow]`. CI also runs the tests on Linux,
macOS and Windows, checks the MSRV (`cargo +1.90 check --workspace --all-targets`) and builds the
WebAssembly package.

Other tasks are `cargo run -p xtask -- <task>`, the same on every OS: `schema` (regenerate the
event and status schemas after changing them), `wasm`, `msrv`, `features`, `fuzz`, `mutants`.
`ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` regenerates
`docs/reference/cli.md` and `exit-codes.md` after a change to the command line or the error codes.

## Tests against real tools

The test suite never starts a real tool: the `fake` feature's scripted processes stand in. Real
runs are separate and opt-in:

- `cargo run -p xtask -- real-run` generates an Isoloom example (default `hello-stack`) with the
  Isoloom checkout beside this one (`--isoloom <dir>` for another), runs it on this computer's
  Docker (`up`, `status`, `check`, `park`, `resume`, `down`) and keeps the events under
  `target/real-run/`.
- `cargo run -p xtask -- real-run cloud <cloud>` does the same on a cloud account whose credentials
  are in the environment, then tears down and reaps whatever happened. It costs money.
- The manual *Cert — real runner* workflow (`real-runner.yml`) runs Proxmox and AWS from CI, when
  its runner and secrets are configured.

Say in your pull request which real runs you did, if any.

## Commits and pull requests

- One logical change per commit. The subject is a short sentence saying what the change does,
  without a prefix or a final period (`A step's timeout counts time without progress`).
- The body, wrapped at about 76 columns, says why, and what a reader needs to know.
- Add an entry under `## [Unreleased]` in [CHANGELOG.md](CHANGELOG.md) for anything a user or an
  embedder notices, in the right section (`Added`, `Changed`, `Fixed`, `Removed`). Start an entry
  that breaks something [Policies](docs/policies.md#what-is-stable) calls stable with
  **Breaking:**, and update the migration guide.
- Update the docs a change makes wrong. Keep them true of the code.
- Never commit a secret, a `.cargo/config.toml`, or a patched `Cargo.lock`.

## Releases

Maintainers release by tag; see [PUBLISHING.md](PUBLISHING.md).
