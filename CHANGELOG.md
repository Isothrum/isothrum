# Changelog

All notable changes to Isothrum are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Isothrum is the runner (crate and CLI) and the lab-model compiler (drawing → spec)
plus its WASM build. It executes what Isoloom generates; it is shared by the Isothrum
desktop app, the isothrum.com editor and the Cyber CTF launcher.

## [1.0.2] — 2026-10-31

### Fixed

- A hot-added ad-hoc attacker box now shows up in a run's status. The box has no
  Compose service and no manifest machine, so `docker compose ps` never listed it and
  the status carried no access machine: an embedder's attack-box panel stayed on "not
  started" and never offered its shell, though the box was up and usable. The Compose
  driver now probes for the box by its deterministic name (when the lab declares no
  access machine of its own) and reports it as an access machine with its running
  state and its address on each lab network. Targets and run readiness are unaffected.
- A run's `down` now removes the ad-hoc attacker box. The box has no Compose project,
  so the manifest's teardown (which removes the project's own containers) left it
  running — an orphaned container that a workload scan still grouped under the lab, so
  the lab kept showing as running after it was stopped. `down` now reaps the box by its
  deterministic name, best effort.

## [1.0.1] — 2026-10-19

### Added

- The runner can hot-add the learner's attacker box to a running Compose lab that
  declares no on-demand access machine: it starts an ad-hoc container (the learner's
  image, else the default Kali image) on the lab network, reports its address, and
  removes it on detach. A lab that still declares an on-demand access machine keeps
  the existing Compose `access`-profile behavior. Vagrant and cloud targets are
  unchanged (ad-hoc attach there is not yet supported).

## [1.0.0] — 2026-10-18

The first stable release, paired with Isoloom 1.0. The `0.11.x` series on crates.io
were the release candidates. The public runner API changed in this release — see the
old→new symbol table in Isoloom's
[`docs/migrating-to-1.0.md`](https://github.com/isoloom/isoloom/blob/main/docs/migrating-to-1.0.md)
and the note below.

### Added

- **Cloud and the egress seal.** The runner drives the cloud Terraform targets and applies
  the manifest's optional `seal` step once the environment is ready (a second apply that
  drops the default route and limits egress to the lab's own CIDRs). The seal fails
  closed: a failed seal means the run is not ready. Sensitive Terraform outputs are
  masked.
- **A stable `ErrorCode` surface.** The embedder-facing error codes are a
  `#[non_exhaustive]` enum, each with a snake_case serde name that round-trips. An
  exhaustive, compile-guarded test now covers every variant (adding or removing one fails
  to compile), so the codes can't drift.
- **`ssh.run` with a bounded wait.** SSH steps accept a `wait_s` (capped at 1800) for a
  booting cloud VM's first connection, and the Terraform driver copies the generated module
  before applying it.

### Changed

- **Domain-neutral run API.** Isothrum is a general-purpose runner, so the CTF/courses word
  "lab" is gone from the public API and the internal vocabulary:

  | 0.x | 1.0 |
  |---|---|
  | `Lab<'r>` (from `Runner::open`) | `Running<'r>` |
  | `status::Run` (the up/resume result) | `Started` |
  | `LabView` (the hook view) | `RunView` |
  | compiler `LabFile` | `ProjectFile` |
  | CLI `LabArgs` | `TargetArgs` |
  | `LabArgs.lab` (its path field) | `TargetArgs.path` |
  | the `lab` module | `run` |

  The public types are now `Runner` (`open` returns `Running<'r>`), `Running<'r>`,
  `Started` and `RunView<'a>`.
- **Reads the manifest's `environment:` key.** The runner consumes the run manifest's
  renamed top-level provenance key (`lab:` → `environment:`, same shape). The committed
  event schema and the fixtures are regenerated accordingly.

### Notes

- The CLI operations are unchanged in shape: `up` (with `--replace` and `--lifetime`),
  `park`, `resume`, `status`, `check`, `provision` and `down`, run against the manifest
  Isoloom writes at `<run>/.isoloom/<target>/run.json`.
