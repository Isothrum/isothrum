# Publishing Isothrum

The Isothrum crates are released to [crates.io](https://crates.io), so that consumers depend on
published versions rather than git revisions and upgrade on their own schedule, and the `isothrum`
binaries to GitHub releases.

Everything here is licensed Apache-2.0.

## Publishable crates

Published to crates.io, in this order (leaf crates first; each crate's dependencies are published
before it):

1. `isothrum-compiler` — a drawn lab becomes an Isoloom spec.
2. `isothrum-runner` — executes the run manifest Isoloom writes (lifecycle, state, drivers, checks, events).
3. `isothrum-machine` — which Vagrant providers this computer can drive.
4. `isothrum-wasm` — the compiler plus Isoloom's targets, compiled to WebAssembly for the browser editor.
5. `isothrum-cli` — the `isothrum` command-line runner.

Not published (`publish = false`, inherited from `[workspace.package]`):

- `xtask` — the repo's development tasks; of no use to consumers.
- `fuzz` — the fuzz targets; a nightly/libFuzzer workspace, excluded from the main workspace.

## Cross-repo dependency on Isoloom

`isothrum-compiler`, `isothrum-wasm` and `isothrum-runner` depend on the Isoloom crates
(`isoloom-spec`, `isoloom-targets`, `isoloom-manifest`) by version, from crates.io
(`[workspace.dependencies]`). For local development against an Isoloom checkout, a git-ignored
`.cargo/config.toml` patches them to its paths (`[patch.crates-io]`); the committed manifests
never name a path or a git source.

**Therefore the Isoloom crates must be on crates.io at the requested version before Isothrum is
published.** Release Isoloom first, then Isothrum.

## Versioning

All crates share one version through `[workspace.package].version`. Bump that single field; every
crate moves together. The Isoloom `version`s in `[workspace.dependencies]` name the Isoloom
release this one pairs with; Isoloom keeps its own version numbers.

## Authentication: crates.io Trusted Publishing (OIDC)

No long-lived API token is stored. `publish-crates.yml` authenticates with crates.io
[Trusted Publishing](https://crates.io/docs/trusted-publishing): the job declares
`permissions: id-token: write`, the official `rust-lang/crates-io-auth-action` mints a short-lived
token over GitHub's OIDC at run time, and `cargo publish` uses it for that run only.

**One-time setup by the maintainer, per publishable crate:** on crates.io, open the crate's
*Settings → Trusted Publishing* and add a GitHub publisher pointing at:

- repository owner/name: `Isothrum/isothrum`
- workflow filename: `publish-crates.yml`
- environment: `crates-io` (the job runs in that environment)

**First publish of a new crate name.** Trusted Publishing cannot *create* a crate that does not
exist yet, so the very first publish of each new name must be done manually, once:

```
cargo login            # paste a token from https://crates.io/settings/tokens
cargo publish -p <crate>
```

after which the crate exists and the Trusted Publisher (added above) handles every later version
from the workflow. The five crates above are all on crates.io already; only a crate added later
needs this manual first publish.

## Release flow

1. Make sure the Isoloom crates for this release are already on crates.io (see above).
2. Bump `[workspace.package].version` (and the path dependencies' `version` in
   `[workspace.dependencies]`) and, if Isoloom moved, the `isoloom-*` versions. The version follows
   [the policies](docs/policies.md): a change that breaks what they call stable is a major
   (`cargo semver-checks` in CI says so for the Rust API).
3. Regenerate `Cargo.lock` **without** the local `.cargo/config.toml` patch (move it aside), so it
   resolves to crates.io, and check `cargo build --locked` passes.
4. Rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] — <date>` and open a new, empty
   `[Unreleased]` above it. The release notes are taken from that section.
5. Commit, then tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
6. The tag runs two workflows, each behind the release gate (`gate.yml`):
   - `publish-crates.yml` publishes the five crates in order and skips any version already on
     crates.io (idempotent, safe to re-run);
   - `release.yml` checks the tag is the workspace version, builds the `isothrum` binaries (Linux
     x86_64/aarch64 musl, macOS arm64/x86_64, Windows x86_64), and publishes a GitHub release with
     the archives, a CycloneDX SBOM, `SHA256SUMS`, its keyless cosign signature and build
     provenance attestations. Its `release` job runs in the `release` environment.
7. To publish the crates without a tag, run the **Publish crates** workflow from the Actions tab
   (`workflow_dispatch`).

### Dry run

Every crate can be dry-run locally (requires the Isoloom crates to be reachable — either already
on crates.io, or resolved to a local checkout with a `[patch.crates-io]` in a git-ignored
`.cargo/config.toml`):

```
cargo publish --dry-run -p isothrum-compiler
cargo publish --dry-run -p isothrum-runner
cargo publish --dry-run -p isothrum-machine
cargo publish --dry-run -p isothrum-wasm
cargo publish --dry-run -p isothrum-cli
```

## The npm / WebAssembly side

`isothrum-wasm` is also delivered to the browser as a wasm-pack bundle:

```
wasm-pack build crates/isothrum-wasm --target web --out-dir ../../web/pkg --release
```

A site vendors that bundle's JavaScript glue and WebAssembly (`cargo run -p xtask -- wasm
--copy-to <dir>` copies them into its folder); the crates.io release above covers the Rust crate
only. If the bundle is later published to npm it is built from this same crate.
