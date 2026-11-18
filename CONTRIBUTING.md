# Contributing to Isothrum

Thank you for helping. Bug reports, fixes, documentation and new features are welcome. For a
large change, open an issue first so we can agree on the approach. Isothrum stays generic: it runs
any Compose project, Vagrantfile or Terraform module, whoever wrote it, and holds nothing specific
to one product built on it ([what Isothrum is, and what it is not](docs/explanation/boundaries.md)).

By contributing, you agree that your contribution is licensed under the Apache License 2.0, as the
project is. Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).

## Set up

1. Install Rust with [rustup](https://rustup.rs/). The toolchain is pinned by
   `rust-toolchain.toml` (1.99.0, with rustfmt and clippy); rustup installs it on first use.
2. **Clone and build.** Isothrum links no generator's crate (it runs the files, whoever wrote
   them), so a plain clone builds:

   ```sh
   git clone https://github.com/Isothrum/isothrum
   cd isothrum
   cargo build
   ```

   `.cargo/config.toml` holds the repository's own command alias only (`cargo xtask`); a crate
   patch has no place there, and `cargo xtask check` refuses one.

The tools the checks use beyond cargo: `cargo install --locked cargo-nextest cargo-machete
cargo-deny cargo-semver-checks typos-cli`, Node (the ratchet runs jscpd through `npx`), the
minimum toolchain (`rustup toolchain install 1.90 --profile minimal`), and for the Windows clippy
`rustup target add x86_64-pc-windows-gnu` with a MinGW-w64 C compiler on `PATH`
(`x86_64-w64-mingw32-gcc`, or llvm-mingw's `x86_64-w64-mingw32-clang`).

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

## Checks

One command runs every check a change must pass, in this repository:

```sh
cargo xtask check          # every step
cargo xtask check --fast   # the short list while you work: fetch, fmt, clippy, test, windows-gnu
```

Each step is named and runs whatever the others did; the end is one line per step and a non-zero
exit when any is red. A step whose tool isn't installed is red, naming what to install: a check
that didn't run never passes for one that did. The steps, in order:

| Step | What it runs |
|---|---|
| `fetch` | `cargo fetch` |
| `generator-free` | `Cargo.lock` lists no generator's crate, and `.cargo/config.toml` patches none in |
| `fmt` | `cargo fmt --all --check` |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` |
| `test` | `cargo xtask test`: nextest, then the doctests; fails on what they leave in temp |
| `doc` | `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` |
| `quality` | `cargo xtask quality`: the clean-code ratchet (`quality-baseline.json`) |
| `regenerate` | `.githooks/regenerate --check`: the generated schemas and reference pages are current |
| `typos` | `typos` |
| `machete` | `cargo machete` |
| `deny` | `cargo deny --all-features check` |
| `msrv` | `cargo +1.90 check --workspace --all-targets` |
| `windows-gnu` | `cargo clippy --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings` |
| `coverage` | the tests' line coverage under cargo-llvm-cov, at least `COVERAGE_FLOOR` in `xtask/src/check.rs` (about a point under dev's, raised as it rises) |
| `semver` | `cargo semver-checks` against each crate's latest release on crates.io |
| `hooks` | shellcheck over the git hooks, then their own test (needs gitleaks) |
| `windows-binary` | Windows only: the release executable needs no Visual C++ runtime |

CI calls these steps and nothing else. On a pull request into `dev`,
[`ci.yml`](.github/workflows/ci.yml) runs the short list (`--fast`) on Linux, macOS and Windows.
Into `main`, [`gate.yml`](.github/workflows/gate.yml) runs every step, each in its own job. The
tests that need a real VM run elsewhere, not on GitHub's runners.

Every lint in the workspace's `[lints]` is at deny: a finding fails the build. An exception is a
local `#[expect(lint, reason = "...")]`, never an `#[allow]`. A process starts through the runner's process
module only (`disallowed-methods` in clippy.toml): an adapter that must start one itself says why
in its `#[expect]`. The xtask's `guardrails` tests keep the rest: no Isothrum crate depends on a
generator's crate, no file under `crates/` or `docs/` names one or uses an application's words, a tool's output is parsed in its adapter, a `-c` script
comes from `driver::shells` and chains nothing with `&&`, and every capability a driver claims
names the test that shows it reach its tool.

Other tasks are `cargo xtask <task>`, the same on every OS: `schema` (regenerate the event and
status schemas after changing them), `msrv`, `features`, `mutants`, `real-run`, `e2e`.
`ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` regenerates
`docs/reference/cli.md` and `exit-codes.md` after a change to the command line or the error codes.

## Tests

A test proves one behaviour that a user, an embedder (an application linking the crates, or
driving the CLI or the service) or a contract (the events, the status, the exit codes, the
protocol) depends on, and proves it once. A test that can't fail, or that proves what another
already does, is deleted; two that differ only in their input are one table of cases.

### Where they live

- **Unit tests** are in the module they test, at its end: `#[cfg(test)] mod tests { use super::*; … }`.
  The size rule of `cargo xtask quality` doesn't count that block.
- When a module's tests outgrow it, they move to `<module>/tests.rs`, declared
  `#[cfg(test)] mod tests;` (Rust finds it there: no `#[path]`), and split by theme into
  `<module>/tests/<theme>.rs` (`run/tests/refusals.rs`, `store/db/tests/crash.rs`). A test file
  keeps under the size rule's 400 lines as a source file does. No `*_tests.rs`.
- **Integration tests** are in the crate's `tests/<subject>.rs` and use only what the crate
  exports: the runner's API (`runner_api.rs`), the CLI's binary (`failures.rs`), the service
  over its protocol (`client_api.rs`). One file per subject, named for it.
- **Helpers.** What the tests of more than one crate share is in `isothrum-test-support`
  (process guards, `wait_until`, `TempHome`, `write_executable`, `assert_every!`). A helper one
  crate's tests share stays in that crate: `testing.rs` beside its unit tests, `tests/common/mod.rs`
  for its integration tests. The scripted processes are the runner's `fake` feature (`fake.rs`),
  since they implement its `ProcessRunner`.

The xtask's `guardrails::layout` test refuses a `#[path]`, a `*_tests.rs` and a `test_` prefix.

### Names

A test's name is the sentence it proves, in snake case, about the behaviour rather than the
function: `a_port_held_on_all_interfaces_is_not_free`, `down_of_a_run_never_started_is_a_no_op`.
No `test_` prefix, no `works`, `ok` or `basic`: when the test fails, its name says what broke.

### How they are written

- Script the tools a test's code runs with `FakeProcesses::script()` (`isothrum-runner`'s `fake`
  feature): each `expect("docker compose ps", reply)` names a program and the arguments its call
  starts with. A call the script doesn't name fails the test, an expectation never met fails it
  when the fake is dropped, and of two that match a call the one naming more arguments answers,
  so the order of a script never matters. Nothing is answered for you: a test of a running Docker
  answers `docker info` itself. Read what was run with `fake.made("docker compose")` and
  `CallView` (`flag("--profile")`, `has("-d")`), not by matching the quoted command line.
  `FakeProcesses::model` is for a model of an engine that answers whatever it is asked (its
  state changes with the calls, or one call is answered differently over time), and adds nothing
  either. These two are the only ways a test stands in for a tool.
- A test that starts a real process holds it in `isothrum-test-support`'s `ChildGuard` (spawned in
  its own process group, killed and reaped when dropped) or, for a process it learnt the id of,
  `PidGuard`; a helper script's loop is bounded. Nothing a test starts outlives it.
- Wait for a condition, never for a time: Tokio's paused clock (`start_paused = true`) for code
  on Tokio's timers, `isothrum_test_support::wait_until` with a deadline for real processes. The
  deadline is generous, there only to stop a hang, never to time what it waits for. A `sleep` that
  is no wait (a stand-in that hangs or answers late, the paused clock, the poll inside a bounded
  wait) is counted in `xtask/src/guardrails/flakes.rs` with its reason, so a new one fails.
- A free port is held from the moment it is picked until it is used: bind to port 0 and hand the
  listener (or its port, while the listener is held) to what uses it. A port read off a listener
  and let go is anyone's, and a test beside it takes it first under load. The same guardrail
  fails a listener on port 0 dropped (or read and let go) right after its port is read.
- A test gets its host as an input (`TEST_HOST`, `TEST_ARCH`, a `Host` value), never this
  machine's; a command it spawns gets a `TempHome` (home, XDG and profile folders, `LC_ALL=C`).
  Each test owns its folders and its environment: it never changes this process's environment
  (the variables are every test's), and gives what it spawns the variables it needs.
- A program a test runs (a stand-in tool) is written with `isothrum_test_support::write_executable`,
  never by the test's own process: a file it has open for writing is open in every process another
  test thread forks meanwhile, and running it then fails with "Text file busy". It is written once
  and found there by every later test and run (`isothrum_test_support::installed_executable`,
  given `env!("CARGO_TARGET_TMPDIR")`), never written fresh by each test: macOS checks every new
  program the first time it runs, which takes seconds on a loaded machine. The tests still writing
  their own are counted in `xtask/src/guardrails/flakes.rs`, so a new one fails.
- Cases of one behaviour are one table, `for (input, expected) in [...]`, the case named in the
  assert's message; a value many tests build is built by one builder or fixture, not copied.
- A test's folder is a `tempfile::TempDir` it holds to its end, never a fixed path.
- `assert_every!` over a collection fails when it is empty, where `iter().all(..)` passes.
- An error is asserted by its code or its message, never by `is_err()` alone.

### Against real tools

The test suite never starts a real tool: the `fake` feature's scripted processes stand in. Real
runs are separate and opt-in:

- `cargo run -p xtask -- real-run` generates an example (default `hello-stack`) with a
  generator's checkout beside this one (`--help` says which, and how to name another), runs it on this computer's
  Docker (`up`, `status`, `check`, `park`, `resume`, `down`) and keeps the events under
  `target/real-run/`.
- `cargo test -p isothrum-cli --test real_run -- --ignored the_compose_example` runs `examples/compose-web-cache` on
  this computer's Docker, as its README does. Every example's README commands also run on a model
  of its tool in the ordinary tests (`commands/tests/examples.rs`): change an example's README and
  that test together.
- `cargo run -p xtask -- real-run cloud <cloud>` does the same on a cloud account whose credentials
  are in the environment, then tears down and reaps whatever happened. It costs money.
- `cargo xtask e2e --tier machine` runs isothrum-machine's `real` tests (`crates/isothrum-machine/tests/real/`)
  against this computer's own tools, through the crate's public API: each provider's detection
  and readiness, the engine and hypervisor self-tests, a start of the running engine, the tool
  path, the network, the OVF Tool, the terminal; and, on a clean machine with its owner there for
  the administrator prompts, each missing tool `ISOTHRUM_E2E_INSTALL` names (`docker,vagrant`,
  or `all`, the tools a setup offers there) installed for real, as a setup installs it.
  Without a desktop session, `ISOTHRUM_E2E_ELEVATION=pkexec` still elevates through pkexec,
  as a setup does, where a polkit rule lets the test user through without a prompt (a test VM's).
  `--tier server` runs them against the test
  servers the `ISOTHRUM_TEST_*` variables name (`tests/real/server.rs` lists them): an SSH
  server, a Proxmox VE node, an ESXi host. Each test runs alone and leaves the machine and the
  server as it found them; one whose tool or server isn't there skips, saying why, and never
  fails. `--results <file>` appends a JSON line per test (`suite`, `tier`, `case`, `result`:
  `pass`, `fail` or `skip`, `reason`, `seconds`, `log`), its log beside the file. On a machine
  without Rust's toolchain (a test VM), `--exe <file>` runs the `real` tests' executable built
  elsewhere (`cargo test --no-run`, cross-built for Windows) in place of `cargo test`, and the
  xtask built for that machine runs them.
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
