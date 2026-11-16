# Publishing Isothrum

The Isothrum crates are released to [crates.io](https://crates.io), so that consumers depend on
published versions rather than git revisions and upgrade on their own schedule, and the `isothrum`
binaries to GitHub releases.

Everything here is licensed Apache-2.0.

## Publishable crates

Published to crates.io, in this order (leaf crates first; each crate's dependencies are published
before it):

1. `isothrum-runner` — executes the run manifest Isoloom writes (lifecycle, state, drivers, checks, events).
2. `isothrum-client` — the runner service's client (depends on the runner).
3. `isothrum-machine` — which Vagrant providers this computer can drive (depends on the runner).
4. `isothrum-service` — the runner service (depends on the runner and the client).
5. `isothrum-cli` — the `isothrum` command-line runner (depends on the runner, the client and the
   service).

Not published (`publish = false`, inherited from `[workspace.package]`):

- `xtask` — the repo's development tasks; of no use to consumers.

## No dependency on Isoloom

Isothrum links no Isoloom crate (`cargo xtask check`'s `isoloom-free` step refuses one), so its
crates publish whatever Isoloom has released. The products' release train still releases Isoloom
first, then Isothrum (the workspace's `./ws release`).

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
from the workflow. `isothrum-runner`, `isothrum-machine` and `isothrum-cli` are on crates.io
already; `isothrum-client` and `isothrum-service` are not yet: each needs this manual first
publish (then its Trusted Publisher) before the workflow can publish it.

## Release flow

1. Make sure every crate above exists on crates.io (a new name's first publish is by hand, above).
2. Bump `[workspace.package].version` (and the path dependencies' `version` in
   `[workspace.dependencies]`). The version follows
   [the policies](docs/policies.md): a change that breaks what they call stable is a major
   (`cargo semver-checks` in CI says so for the Rust API).
3. Check `cargo build --locked` passes, so `Cargo.lock` is the one the release builds with.
4. Rename `## [Unreleased]` in `CHANGELOG.md` to `## [X.Y.Z] — <date>` and open a new, empty
   `[Unreleased]` above it. The release notes are taken from that section.
5. Commit, then tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
6. The tag runs two workflows, each behind the release gate (`gate.yml`):
   - `publish-crates.yml` publishes the five crates in order and skips any version already on
     crates.io (idempotent, safe to re-run);
   - `release.yml` checks the tag is the workspace version, builds the `isothrum` binaries (Linux
     x86_64/aarch64 musl, macOS arm64/x86_64, Windows x86_64: its C runtime linked in,
     `-C target-feature=+crt-static`, the build failing when `isothrum.exe` still imports
     `VCRUNTIME*.dll` or `MSVCP*.dll`, which CI's `windows-binary` job checks on every push too),
     and publishes a GitHub release with
     the archives, a CycloneDX SBOM, `SHA256SUMS`, its keyless cosign signature and build
     provenance attestations. Its `release` job runs in the `release` environment.
7. To publish the crates without a tag, run the **Publish crates** workflow from the Actions tab
   (`workflow_dispatch`).

### Dry run

`isothrum-runner` can be dry-run locally as it is; each crate after it depends on crates that are
not on crates.io at the new version until they are published, so point those at the checkout with
a `[patch.crates-io]` passed to cargo (`cargo --config patch.toml publish --dry-run -p <crate>`):

```
cargo publish --dry-run -p isothrum-runner
cargo publish --dry-run -p isothrum-client
cargo publish --dry-run -p isothrum-machine
cargo publish --dry-run -p isothrum-service
cargo publish --dry-run -p isothrum-cli
```

