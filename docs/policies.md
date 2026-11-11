# Policies

What Isothrum promises between releases, and how each promise is versioned.

## Versioning

All published crates share one version (`[workspace.package].version`) and follow
[Semantic Versioning](https://semver.org/). A release that breaks anything listed under
[What is stable](#what-is-stable) is a new major version; one that only adds is a minor; one that
only fixes is a patch. Every change is recorded in [CHANGELOG.md](../CHANGELOG.md), breaking ones
marked **Breaking**.

## What is stable

| Surface | Stable within a major | How it is versioned and checked |
|---|---|---|
| **Rust API** of `isothrum-runner`, `isothrum-compiler`, `isothrum-machine` and `isothrum-wasm` | Public items, their signatures and behaviour, every feature included (`fake` too). Not stable: `#[doc(hidden)]` items and the `fuzzing` feature. | The crate version. CI runs `cargo semver-checks` against the last release on crates.io. `#[non_exhaustive]` types may gain variants and fields in a minor release. |
| **The re-exported `isoloom_manifest`** (`isothrum_runner::isoloom_manifest`) | As stable as the Isoloom release it pins. | Raising it to a new major of `isoloom-manifest` is a major release of Isothrum. |
| **CLI**: commands, flags, exit codes | Names, meanings and exit codes ([Exit codes](reference/exit-codes.md)). | The crate version. New commands and flags may come in a minor release. |
| **CLI output for machines**: `--json` events, `status --json` | The event schema and the status schema below. | As below. |
| **CLI output for machines**: `console --json`, `screen --json`, `key --json` | Field names and meanings, as [One JSON document](#one-json-document) lists them. | Additions in minor releases; removals and renames in a major. |
| **CLI output for people** (text without `--json`) | Not stable. | Don't parse it. |
| **JSON event schema** ([`events-v0.schema.json`](../crates/isothrum-runner/schema/events-v0.schema.json)) | Event kinds, field names and meanings. | The event's `v` (0). Adding an event kind, an optional field or a code keeps `v`; removing or renaming one, or changing a meaning, raises `v` and is a major release. Read the stream as [Event stream](reference/events.md#reading-the-stream) says. |
| **Status schema** ([`status.schema.json`](../crates/isothrum-runner/schema/status.schema.json)) | Field names and meanings. | Additions in minor releases; removals and renames in a major. |
| **`ErrorCode` and `WarningCode` values** (`code` in events, `last.code` in the record) | A code's string and meaning. | A code is never renamed or reused within a major. New codes may come in a minor release, so handle an unknown code as a generic failure or warning. Removing a code is a major release. |
| **`record.json`** ([State layout](reference/state.md#recordjson)) | Isothrum reads every record an earlier release of the same major wrote. | The record's `version` (0), raised when a release writes records an earlier one would misread. Going back to an earlier release after a newer one wrote the home is not supported. |
| **The rest of the home** (the run's state folder, file names, the event log) | Not an interface. | The tools' own files (Terraform state, Vagrant's state) follow those tools' rules. |

### One JSON document

With `--json`, these commands print one JSON object on standard output (a failure is an `error`
event, as for any command):

- **`console`**: the command that opens the console, `ShellCommand`'s fields: `program`, `args`
  (a list), `cwd` (the folder to run it in) and `env` (a list of `[name, value]` pairs to run it
  with, besides the caller's own variables).
- **`screen`**: `machine`; `protocol` (`vnc` or `rdp`); `address`, the loopback `host:port` a
  viewer connects to, or `null`; `websocket`, Proxmox VE's VNC websocket URL (it holds the
  session's ticket), or `null`; `user`, the account to sign in as, or `null`; `password`, only
  with `--show-password`, else `null`; `tunnelled`, whether the command holds an SSH tunnel open
  for it; `note`, what the manifest says of it, or `null`.
- **`key`**: `public_key`, the runner's SSH public key, one line.

## Run manifest versions

Isothrum reads the run manifest Isoloom writes (`run.json`), whose `manifest_version` says its
contract. Isothrum reads exactly the one version that the `isoloom-manifest` crate it is built
with accepts; any other fails with `manifest_version` before anything runs. What may change
within a version, and what bumps it, is Isoloom's policy (`isoloom_manifest::COMPATIBILITY`, also
stated in the [run manifest schema](https://raw.githubusercontent.com/isoloom/isoloom/main/schemas/run-manifest-v1.schema.json)):
within a version changes are additive, but the reader is strict, so a manifest that uses a field
added in a minor Isoloom release needs an Isothrum built with that release.

| Isothrum | `isoloom-manifest` it builds with | Reads `manifest_version` |
|---|---|---|
| 1.0.0 to 1.0.5 | 1.0.x (1.0.9 for 1.0.5) | 0 |
| 2.0.0 to 2.1.0 | 2.0.x (2.0.0 for 2.0.0 and 2.1.0) | 1 |

Each Isothrum release names the Isoloom release it pairs with in `Cargo.toml`
(`[workspace.dependencies]`). Moving to a new `manifest_version` means older manifests are
refused: that is a major release of Isothrum.

## Minimum supported Rust version (MSRV)

- The crates declare `rust-version = "1.90"`; CI checks the workspace builds with Rust 1.90.
- Development, CI and the release binaries use the toolchain pinned in `rust-toolchain.toml`
  (1.99.0), which is newer: the MSRV is what a crate's users need, the pin is what we build with.
- The MSRV is raised only when a dependency or a language feature needs it, to a Rust release at
  least six months old. Raising it is a **minor** release, never a patch, and the CHANGELOG says
  so.

## Supported releases

- The latest release of the current major receives bug and security fixes, as new patch or minor
  releases. Fixes are not backported to earlier minors: upgrade within the major.
- When a new major is released, the last release of the previous major receives fixes for
  security vulnerabilities for three months.
- How to report a vulnerability, and the response times: [SECURITY.md](../SECURITY.md).
