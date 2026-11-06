# Publishing Isothrum

The Isothrum library crates are released to [crates.io](https://crates.io) so that consumers
(the desktop app, the website, the Cyber CTF launcher and any external user) depend on published
versions rather than git revisions. That way Isoloom's and Isothrum's history can be squashed at
1.0 without breaking anyone, and each consumer upgrades on its own schedule.

Everything here is licensed Apache-2.0.

## Publishable crates

Published to crates.io, in this order (leaf crates first; each crate's dependencies are published
before it):

1. `isothrum-compiler` — a drawn lab becomes an Isoloom spec.
2. `isothrum-runner` — executes the run manifest Isoloom writes (lifecycle, state, drivers, checks, events).
3. `isothrum-wasm` — the compiler plus Isoloom's targets, compiled to WebAssembly for the browser editor.
4. `isothrum-cli` — the `isothrum` command-line runner.

Not published (`publish = false`, inherited from `[workspace.package]`):

- `xtask` — the repo's development tasks; of no use to consumers.
- `fuzz` — the fuzz targets; a nightly/libFuzzer workspace, excluded from the main workspace.
- `apps/desktop/src-tauri` — the desktop application; it ships as a signed installer from
  `release.yml`, not as a library.

## Cross-repo dependency on Isoloom

`isothrum-compiler`, `isothrum-wasm` and `isothrum-runner` depend on the Isoloom crates
(`isoloom-spec`, `isoloom-targets`, `isoloom-manifest`). In the committed manifests these carry
both a git `rev` (for local development, via `cargo xtask dev-link`) **and** a `version`. On
publish, cargo drops the git source and keeps the version, so the published crates depend on
`isoloom-* = "<version>"` from crates.io.

**Therefore the Isoloom crates must be on crates.io at the requested version before Isothrum is
published.** Release Isoloom first, then Isothrum.

## Versioning

All crates share one version through `[workspace.package].version`. Bump that single field; every
crate moves together. Keep the Isoloom `version` in `[workspace.dependencies]` in step with the
Isoloom release the git `rev` points at.

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
from the workflow. **New vs existing:** none of the four crates are on crates.io yet, so all of
them (`isothrum-compiler`, `isothrum-runner`, `isothrum-wasm`, `isothrum-cli`) are new names and
need the one-time manual first publish for the `v0.11.0` release candidate (the forward RC the
release plan settled on; the workspace is at 0.11.0). Confirm each name is free (or already owned)
before the first release. From `v0.11.1` onward the tag workflow does everything.

## Release flow

1. Make sure the Isoloom crates for this release are already on crates.io (see above).
2. Bump `[workspace.package].version` and, if Isoloom moved, the `isoloom-*` `rev`/`version` in
   `[workspace.dependencies]`.
3. Commit, then tag: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. The tag runs `.github/workflows/publish-crates.yml`, which publishes the four crates in order
   and skips any version already on crates.io (idempotent — safe to re-run). The same tag also
   runs `release.yml` for the desktop installers.
5. To publish without a tag, run the **Publish crates** workflow from the Actions tab
   (`workflow_dispatch`).

### Dry run

Every crate can be dry-run locally (requires the Isoloom crates to be reachable — either already
on crates.io, or resolved to a local checkout with a `[patch.crates-io]` in a git-ignored
`.cargo/config.toml`):

```
cargo publish --dry-run -p isothrum-compiler
cargo publish --dry-run -p isothrum-runner
cargo publish --dry-run -p isothrum-wasm
cargo publish --dry-run -p isothrum-cli
```

## The npm / WebAssembly side

`isothrum-wasm` is also delivered to the browser as a wasm-pack bundle:

```
wasm-pack build crates/isothrum-wasm --target web --out-dir ../../web/pkg --release
```

Today isothrum.com vendors that bundle under `public/wasm/`; the crates.io release above covers
the Rust crate only. If the bundle is later published to npm it is built from this same crate.
The launcher's shared UI package, `@cyber-courses/ui`, is a separate concern and already ships
through GitHub Packages, not crates.io or public npm.
