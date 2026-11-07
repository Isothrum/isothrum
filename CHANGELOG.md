# Changelog

All notable changes to Isothrum are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Isothrum is the runner (crate and CLI) and the lab-model compiler (drawing → spec)
plus its WASM build. It executes what Isoloom generates; it is shared by the Isothrum
desktop app, the isothrum.com editor and the Cyber CTF launcher.

## [Unreleased]

### Fixed

- **A lab on UTM reads as running once it is up.** UTM's Vagrant plugin reports a running VM as
  `started` (UTM has no running state of its own), which the runner left as an unknown state, so
  a lab that had come up on UTM still read as not running. `started` now reads as running.

## [1.0.4] — 2026-11-07

`isothrum-machine`, the shared machine/setup layer, was first published to crates.io
standalone at 1.0.3. From this release it is versioned and published with the rest of
the workspace, so it moves straight to 1.0.4 alongside the other crates.

### Added

- **`isothrum-machine` reaches launcher parity and becomes publishable.** The crate now
  covers the launcher's machine-layer capabilities while staying no-unsafe: an
  `ElevatedBoot` injection point runs a VM self-test's `vagrant up` + readiness probe as
  one elevated batch, an injected `may_manage_hyperv` folds the needs-admin reason into
  Hyper-V's status without computing the privileged Win32 token check in the crate,
  Docker engine start/switch (`docker_start_engine`/`docker_use_engine`) are ported over
  the runner's `ProcessRunner`, granular Docker/VM self-tests are exposed, and
  `docker_denied_hint` is now a `DockerDenied` code (`SocketRefused`/`PipeAccessDenied`).
  The crate is flipped to `publish = true` and slotted into the release publish list after
  its `isothrum-runner` dependency, so it ships with the workspace.
- **A release tag is now gated on tests and the quality ratchet.** A reusable `gate.yml`
  (`cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace --locked`,
  `cargo run -p xtask -- quality`) now blocks both the crates.io publish and the desktop
  installer workflows, so a `v*` tag that fails any of them publishes nothing. Push/PR CI
  stays off as configured.

### Fixed

- **Overlapping operations on one run no longer clobber each other's in-flight entry or
  free each other's port reservations.** `operate()` entered the in-flight map and reserved
  ports before taking the per-run lock; two concurrent ops on the same run now enter under
  the lock instead.
- **A Vagrant lab's ephemeral published port with no preferred host number is now
  reported.** Vagrant status skipped a published port whose `publish.preferred` was `None`,
  leaving its `published`/host templates resolving `Unbound`; it now gates on `publish` and
  takes the host port from the pick.
- **Two local QEMU docker-vm labs no longer collide on the helper VM's SSH forward.**
  `vagrant-qemu` forwarded the helper VM's guest SSH to a fixed host port (`50022`) with
  weak auto-correct, so two labs — or a half-created VM left by a failed start — collided.
  On QEMU only, the SSH forward now reserves a free loopback port (held under
  `<guest>/ssh`), rides in `ISOLOOM_HOSTPORTS` to the Vagrantfile, and is checked free
  before start, exactly as a published ephemeral port is.

## [1.0.3] — 2026-11-05

### Added

- **A local VM lab's published ports now get a free host port, picked by the runner.** On a
  fresh Vagrant run, each ephemeral published port is forwarded to a free loopback port (or its
  own `publish` number under `ISOLOOM_PUBLISH_FIXED`), reserved and persisted in the run record
  so park/resume/status keep the same port, passed to Vagrant in `ISOLOOM_HOSTPORTS`, reported in
  the lab's status and URL, and released on teardown. A privileged port (`80`/`443`) no longer
  needs admin rights to forward, and two labs that publish the same port no longer collide.

## [1.0.2] — 2026-10-31

### Fixed

- A hot-added ad-hoc attacker box now shows up in a run's status. The box has no
  Compose service and no manifest machine, so `docker compose ps` never listed it and
  the status carried no access machine: an embedder's attack-box panel stayed on "not
  started" and never offered its shell, though the box was up and usable. The Compose
  driver now probes for the box by its deterministic name (when the lab declares no
  access machine of its own) and reports it as an access machine with its running
  state and its address on each lab network. Targets and run readiness are unaffected.
- A run's `down` and `park` now remove the ad-hoc attacker box. The box has no Compose
  project, so the manifest's teardown (which removes or stops the project's own
  containers) left it running — an orphaned container that a workload scan still grouped
  under the lab, so the lab kept showing as running after it was stopped or parked. Both
  now reap the box by its deterministic name, best effort; a resumed or relaunched lab
  re-attaches one.

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
