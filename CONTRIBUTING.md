# Contributing to Isothrum

Thank you for helping. Bug reports, fixes, documentation and new features are welcome. For a
large change, open an issue first so we can agree on the approach. Isothrum stays generic: it runs
any run manifest Isoloom writes, and holds nothing specific to one product built on it.

By contributing, you agree that your contribution is licensed under the Apache License 2.0, as the
project is. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Set up

1. Install Rust with [rustup](https://rustup.rs/). The toolchain is pinned by
   `rust-toolchain.toml` (1.99.0, with rustfmt and clippy); rustup installs it on first use.
2. **Clone and build.** `Cargo.toml` pins the Isoloom crates on crates.io (2.0.0), so a plain
   clone builds:

   ```sh
   git clone https://github.com/Isothrum/isothrum
   cd isothrum
   cargo build
   ```

   **A change that needs unreleased Isoloom API** builds against an Isoloom checkout instead.
   Clone both side by side, Isoloom at its `main`:

   ```sh
   git clone https://github.com/isoloom/isoloom
   git clone https://github.com/Isothrum/isothrum
   ```

   Then write `isothrum/.cargo/config.toml` (git-ignored: never commit it), patching every Isoloom
   crate the build reads to that checkout:

   ```toml
   [patch.crates-io]
   isoloom-ansible = { path = "../isoloom/crates/backends/isoloom-ansible" }
   isoloom-compose = { path = "../isoloom/crates/backends/isoloom-compose" }
   isoloom-kubernetes = { path = "../isoloom/crates/backends/isoloom-kubernetes" }
   isoloom-terraform = { path = "../isoloom/crates/backends/isoloom-terraform" }
   isoloom-vagrant = { path = "../isoloom/crates/backends/isoloom-vagrant" }
   isoloom-catalogue = { path = "../isoloom/crates/core/isoloom-catalogue" }
   isoloom-manifest = { path = "../isoloom/crates/core/isoloom-manifest" }
   isoloom-plan = { path = "../isoloom/crates/core/isoloom-plan" }
   isoloom-render = { path = "../isoloom/crates/core/isoloom-render" }
   isoloom-spec = { path = "../isoloom/crates/core/isoloom-spec" }
   isoloom-targets = { path = "../isoloom/crates/targets/isoloom-targets" }
   ```

   A path is read from the folder that holds `.cargo/`; the fuzz crate (`fuzz/`) reads the same
   file. Then build:

   ```sh
   cd isothrum
   cargo build
   ```

   The patched build rewrites `Cargo.lock` and `fuzz/Cargo.lock` for the local crates: don't
   commit those changes, and run the gates below without `--locked` (it refuses the rewrite).
   CI builds against crates.io (`--locked`, no patch), so such a change waits for the Isoloom
   release that has the API; the release step raises the pins to it.

Optional tools the gates use: `cargo install cargo-nextest cargo-machete cargo-deny
cargo-semver-checks typos-cli`, Node (the ratchet runs jscpd through `npx`), and `wasm-pack` for
the WebAssembly build.

## Git hooks

`.githooks/` holds the repository's git hooks; `scripts/setup-hooks.sh` turns them on
(`git config core.hooksPath .githooks`, once per clone; the clone's worktrees share it). They are
POSIX sh and quick:

- `commit-msg` refuses a message with an assistant's attribution (`Co-Authored-By`,
  `Claude-Session`, "Generated with", 🤖): the history carries none.
- `pre-commit` refuses a staged file over 5 MB that Git LFS doesn't hold, and runs rustfmt over
  the staged Rust.
- `pre-push` runs [gitleaks](https://github.com/gitleaks/gitleaks) over the commits being pushed
  (`brew install gitleaks`; without it the push is refused, not left unscanned), the attribution
  check over them, then `cargo fmt --all --check` and `typos`, in a few seconds. The other gates
  stay in CI.

- `regenerate` writes the files generated from the code again (the event and status schemas,
  `docs/reference/cli.md` and `exit-codes.md`); `--check` fails naming it when one is stale (a
  gate). Opt in to running it before each commit, which refuses a commit leaving one stale:
  `git config hooks.regenerate true`.

`.githooks/test.sh` tests them in a throwaway repository; CI's `hooks` job runs it.
`git commit --no-verify` skips a hook when you must; the push still checks the commits.

## Gates

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs these on every pull request; run
them before you push:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- test            # nextest, then doctests; fails on what they leave in temp
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
cargo run -p xtask -- quality          # the clean-code ratchet (quality-baseline.json)
.githooks/regenerate --check           # the generated schemas and reference pages are current
cargo deny check
cargo machete
typos
cargo run -p xtask -- fuzz --check     # the fuzz crate (fuzz/, a workspace of its own): format, lints, cargo-deny
cargo semver-checks --workspace --exclude isothrum-cli --exclude xtask
```

Every lint in the workspace's `[lints]` is at deny: a finding fails the build. An exception is a
local `#[expect(lint, reason = "...")]`, never an `#[allow]`. A process starts through the runner's process
module only (`disallowed-methods` in clippy.toml): an adapter that must start one itself says why
in its `#[expect]`. The xtask's `guardrails` tests keep the rest: no Isothrum crate depends on an
Isoloom crate beyond the listed edges, a tool's output is parsed in its adapter, a `-c` script
comes from `driver::shells` and chains nothing with `&&`, and every capability a driver claims
names the test that shows it reach its tool. CI also runs the tests on Linux,
macOS and Windows, checks the MSRV (`cargo +1.90 check --workspace --all-targets`) and builds the
WebAssembly package.

Other tasks are `cargo run -p xtask -- <task>`, the same on every OS: `schema` (regenerate the
event and status schemas after changing them), `wasm`, `msrv`, `features`, `fuzz`, `mutants`.
`ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` regenerates
`docs/reference/cli.md` and `exit-codes.md` after a change to the command line or the error codes.

## Writing tests

- Script the tools a test's code runs with `FakeProcesses::script()` (`isothrum-runner`'s `fake`
  feature): each `expect("docker compose ps", reply)` names a program and the arguments its call
  starts with. A call the script doesn't name fails the test, an expectation never met fails it
  when the fake is dropped, and of two that match a call the one naming more arguments answers,
  so the order of a script never matters. Nothing is answered for you: a test of a running Docker
  answers `docker info` itself. Read what was run with `fake.made("docker compose")` and
  `CallView` (`flag("--profile")`, `has("-d")`), not by matching the quoted command line.
  `FakeProcesses::model` is for a model of an engine that answers whatever it is asked, and adds
  nothing either. `FakeProcesses::new` (a model whose silent `docker info` prints a version) and
  `FakeProcesses::rules` (a lenient script: first substring match, anything else succeeds) are
  the old forms, kept until their tests move.
- A test that starts a real process holds it in `isothrum-test-support`'s `ChildGuard` (spawned in
  its own process group, killed and reaped when dropped) or, for a process it learnt the id of,
  `PidGuard`; a helper script's loop is bounded. Nothing a test starts outlives it.
- Wait for a condition, never for a time: Tokio's paused clock (`start_paused = true`) for code
  on Tokio's timers, `isothrum_test_support::wait_until` with a deadline for real processes.
- A test gets its host as an input (`TEST_HOST`, `TEST_ARCH`, a `Host` value), never this
  machine's; a command it spawns gets a `TempHome` (home, XDG and profile folders, `LC_ALL=C`).
- A program a test runs (a stand-in tool) is written with `isothrum_test_support::write_executable`,
  never by the test's own process: a file it has open for writing is open in every process another
  test thread forks meanwhile, and running it then fails with "Text file busy".
- `assert_every!` over a collection fails when it is empty, where `iter().all(..)` passes.
- An error is asserted by its code or its message, never by `is_err()` alone.

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
