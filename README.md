# Isothrum

Isothrum runs what [Isoloom](https://github.com/isoloom/isoloom) generates. Isoloom is the
converter: it reads one spec and writes the native files of every tool (Compose, Vagrant,
Terraform, cloud-init, Ansible) plus a versioned **run manifest**. Isothrum is the engine that
executes that manifest — bring a lab up, check it, park and resume it, tear it down — and the
compiler that turns a drawn lab into an Isoloom spec.

This repository is **libraries and a CLI only**: the apps and UI components built on them live
in their own repositories, on the crates published from here. Everything here is Apache-2.0.

Current version: **1.0.5** (the workspace version, shared by every crate). See
["Status"](#status) for what is proven by real runs and what is not yet.

## Crates

| Crate | crates.io | What it is |
|---|---|---|
| [`isothrum-runner`](https://crates.io/crates/isothrum-runner) | `isothrum-runner` | Executes the run manifest: lifecycle, state and locks, drivers, secrets, checks, the event stream. |
| [`isothrum-compiler`](https://crates.io/crates/isothrum-compiler) | `isothrum-compiler` | A lab drawn in the editor (machines, the LANs around them, the cables between their ports) becomes an Isoloom spec. |
| [`isothrum-wasm`](https://crates.io/crates/isothrum-wasm) | `isothrum-wasm` | The compiler plus Isoloom's targets, compiled to WebAssembly for the in-browser editor. The same build runs on isothrum.com's server for its MCP tools. |
| [`isothrum-machine`](https://crates.io/crates/isothrum-machine) | `isothrum-machine` | Which Vagrant providers (hypervisors) this computer can drive, and the record format of what a program downloaded for its own operations. |
| [`isothrum-cli`](https://crates.io/crates/isothrum-cli) | `isothrum-cli` | The `isothrum` command-line runner. |

`xtask` and the `fuzz/` targets are not published.

## The runner

The runner reads the run manifest Isoloom writes next to a target's generated files
(`.isoloom/<target>/run.json`) and **reads nothing else of them**. The manifest carries the
native commands in order, the documented driver actions, readiness criteria, outputs (addresses,
host ports, URLs), the check runners, and the slots that inputs and secrets travel through — never
their values. The runner adds what only it can know: the run's name and private state, one
operation per run at a time, the values of params and secrets, and the JSON event stream an
embedder (an app, the `isothrum` CLI) consumes.

The manifest is the cross-repo contract; Isoloom's `isoloom-manifest` crate defines it.

### Lifecycle

`up`, `down`, `park` (`--mode pause|shutdown`, default `shutdown`), `resume`, `update`, `retire`,
`status`, `check`, `provision`, `attach`, `detach`, `console` and `events`, each on a lab folder
(its `.isoloom/<target>/run.json`, or `--target` when the lab was generated for several) or on a
`run.json` given directly. `reap` destroys a cloud's expired runs; `key` prints the runner's SSH
public key.

- `up` on a running run fails `already_running`; on a parked run it resumes it. Leftovers of an
  earlier run are removed first (a `leftovers_removed` warning). `--replace` removes whatever
  exists and starts afresh.
- A run recorded under another `--host-label` fails `running_elsewhere`.
- `update` brings a running run up to date with its regenerated manifest: `up`'s steps run again
  with the params it was started with (a running VM whose boot-time settings changed is restarted
  first). `retire <machine>` takes a machine out of the running run ahead of its removal from the
  manifest, for what the update after wouldn't remove itself.
- `attach` and `detach` hot-add and hot-remove the lab's on-demand access machine; `console
  <machine>` prints the command that opens a console into any machine of the run.
- `down`, `park`, `resume`, `check` and `provision` replay the params the run was started with
  (recorded ones win over any given again). Secret values are never recorded: an operation that
  delivers them again (`provision`, an `update`) takes them again.
- Before `up` and `resume`, every tool the manifest uses on this host runs its version probe and
  must meet its `min_version` — a stopped Docker engine or an old Vagrant fails here, before
  anything is created (`tool_missing`, `tool_too_old`).

### Drivers

One `Driver` per manifest driver (`compose`, `vagrant`, `terraform`), behind a trait; the process
runner and the host ports are traits too, so tests use fakes and never a real tool. The state
store is a plain folder under the runner's home: tests give it a temporary one.

- **`compose`** — container labs on the host's Docker (Compose ≥ 2.17.0). Ports are pinned to free
  loopback ports, or fixed with `ISOLOOM_PUBLISH_FIXED`; every published port is checked free on
  127.0.0.1 right before `up`, inside the run's lock.
- **`vagrant`** — VM labs on a local hypervisor (VirtualBox, Hyper-V, VMware, Parallels, UTM,
  QEMU/libvirt) and on servers (VMware ESXi). The provider is chosen per run (`--provider`) and
  kept for the run's life; Vagrant ≥ 2.4.0. Vagrant's own state lives in the run's state folder
  (`VAGRANT_DOTFILE_PATH`), not next to the Vagrantfile. Hyper-V operations elevate once per
  operation (one UAC prompt) when the account isn't already elevated.
- **`terraform`** — server and cloud labs (Proxmox, the six clouds, cloud-docker, cloud-services).
  State is in the run's folder, providers in a shared plugin cache. On macOS, Terraform runs from
  a digest-pinned `hashicorp/terraform` image; elsewhere the host binary (≥ 1.6).

### Secrets and slots

A slot's value reaches only the steps its delivery names (`env` for Compose, `ISOLOOM_SECRET_*`
or the input's own variable for Vagrant, `TF_VAR_*` / a JSON map entry for Terraform, or over
`ssh.run` for the self-setup cloud path). Every sensitive value is replaced by `***` in events,
logs, error messages and results; a sensitive output shows as `***` and the lab's message never
substitutes it. **No value is ever stored.** A slot may carry a `default` the runner delivers when
it has no value (so a lab started without an input, and every resume, still comes up). Leak tests
assert a delivered value appears in no file under the home and in no event through a whole run,
not even in a failed step's error that printed it.

### SSH relay and slots on servers

The runner owns an SSH key (`<home>/ssh/id_ed25519`, made on first use; `isothrum key` prints its
public half). A run's SSH uses a generated `ssh_config` (the key, the run's `known_hosts`,
`accept-new`, batch mode); `--ssh-jump user@host` (and any manifest jump host) is handed to every
connection with `ssh -J`. On macOS a Vagrant-on-ESXi run gets loopback relays to the ESXi host's
SSH and HTTPS through Apple's `/usr/bin/nc` (macOS Local Network Privacy blocks third-party
binaries), and an `ovftool` wrapper first on `PATH`. `${run.slot}` is the lowest of 1–99 no other
live run on the same place (`--host-label`) holds, kept from the first `up` to `down`
(`slots_full` when none is free).

### The egress seal

`lifecycle.seal` (`{ param, steps }`): once the machines are set up and the ready conditions hold,
the runner sets the param true and runs the seal steps — a cloud's second Terraform apply that
closes the internet to its `internet: false` networks at the network layer (AWS route tables,
Azure NSGs, GCP egress rules, Linode/OCI/DigitalOcean firewalls). A failed seal fails `up`; a
later operation replays the recorded value; a successful `down` forgets it. For `docker-vm` the
seal is none — `internet: false` is sealed inside the VM by the containers' own route sidecars.

### Event stream (version 0)

One JSON object per event, with `v`, `seq` (from 1 per operation), `time` (UTC), `run`, `op` and
`event`: `state`, `step`, `log`, `ready`, `output`, `check`, `warning`, `result`, `error`. Every
operation ends with exactly one `result` or `error`. Warnings are typed (`leftovers_removed`,
`recovering`, `retrying`, `saved_state_discarded`, `check_cleanup_failed`, …).

The schema is committed at
[`crates/isothrum-runner/schema/events-v0.schema.json`](crates/isothrum-runner/schema/events-v0.schema.json),
so embedders in other languages read the events without the crate. `cargo run -p xtask -- schema`
regenerates it; a test fails if it drifts.

### Error codes

`manifest_invalid`, `manifest_version`, `unsupported`, `io`, `locked`, `tool_missing`,
`tool_failed`, `tool_too_old`, `timeout`, `already_running`, `running_elsewhere`, `port_in_use`,
`not_parkable`, `nothing_to_resume`, `not_running`, `not_ready`, `not_sealed`, `unreachable`,
`secret_missing`, `param_invalid`, `run_name`, `provider_refused`, `machine_unknown`,
`slots_full`, `adopt_refused`, `elevation_declined`.

### The runner's home

`<home>` defaults to `$ISOTHRUM_HOME`, else `~/.isothrum`. Each run lives in `<home>/runs/<run>/`:
`record.json` (the manifest it was last brought up or updated from, target, driver, state,
non-secret params, start time, last operation), `lock`, `events.jsonl` (trimmed past 4000 lines), and the run's `${run.state_dir}`.
Files are 0600 and folders 0700 on Unix. Nothing is written in the lab's folder.

## How it depends on Isoloom

`isothrum-runner`, `isothrum-compiler` and `isothrum-wasm` depend on the Isoloom crates
`isoloom-spec`, `isoloom-targets` and `isoloom-manifest`, from crates.io, at the Isoloom release
each Isothrum release pairs with (Isoloom keeps its own version numbers):

```toml
[workspace.dependencies]
isoloom-manifest = "1.0.9"
```

The Isoloom crates for a release must therefore be on crates.io before Isothrum is published
(release Isoloom first). To build against a local Isoloom checkout, patch them in a git-ignored
`.cargo/config.toml` (`[patch.crates-io]`, each crate's `path`). See
[PUBLISHING.md](PUBLISHING.md) for the full flow.

## Install

The `isothrum` CLI:

```
cargo install isothrum-cli
```

Or build from this checkout:

```
cargo build --release -p isothrum-cli   # target/release/isothrum
```

Use the runner as a library:

```
cargo add isothrum-runner
```

## Quickstart

Point Isoloom at a lab folder and generate a target, then run it with `isothrum`:

```
# in the lab folder, with Isoloom installed
isoloom generate --target docker

# bring it up, check it, tear it down
isothrum up
isothrum check
isothrum down
```

`isothrum up` reads `.isoloom/docker/run.json`, starts the lab, pins its published ports and
prints the lab's message with the host ports filled in. `isothrum status` shows the run; `isothrum
park` / `isothrum resume` stop and restart it keeping its state; `isothrum events` prints the
recorded event stream. Add `--json` for the events as JSON lines on stdout. Secrets and inputs are
delivered with `--secret-from-env NAME[=VAR]` (values come only from the environment, never the
command line) and params with `--param NAME=VALUE`.

A VM lab adds `--provider`:

```
isoloom generate --target vagrant
isothrum up --provider virtualbox
```

## Status

- **Proven by real runs:** `docker` (macOS, Linux, Windows — Tier 1); `vagrant` on VirtualBox
  (macOS), libvirt (Linux) and Hyper-V (Windows); `docker-vm` on VirtualBox (macOS); `proxmox` and
  `vagrant/vmware_esxi` from macOS against the lab servers.
- **Generated and validated, not yet run for real:** the six clouds (`cloud-vm`, `cloud-docker`,
  `cloud-services`) — they need budget-capped accounts.
- **RC-level / owed:** the apt-repo Docker bootstrap needs a real apt-guest run on a Linux host to
  record; the RPM family still installs Docker from the pinned install script (an apt-style
  repository is a 1.0 follow-up).

## Development

Rust is pinned by `rust-toolchain.toml` (MSRV 1.90). Development tasks are `cargo xtask` commands,
so they run the same on macOS, Windows and Linux:

```
cargo run -p xtask -- test        # cargo-nextest, then the doctests
cargo run -p xtask -- schema      # regenerate the event schema
cargo run -p xtask -- quality     # the clean-code ratchet (quality-baseline.json)
cargo run -p xtask -- wasm        # build the editor engine into web/pkg/
cargo run -p xtask -- fuzz        # the cargo-fuzz targets
cargo run -p xtask -- mutants     # mutation testing of the compiler and the runner's executor
```

The full gate is Isoloom's `cargo xtask check-all --repos isothrum`: fmt, typos, machete, deny,
clippy on three targets (every workspace lint is at deny, so any finding fails it), `cargo doc` at
`-Dwarnings`, `quality`, msrv, features, fuzz-check and the tests; `--deep` adds fuzz and mutants.
The release gate (`.github/workflows/gate.yml`) runs clippy with `-D warnings` too.

The WebAssembly engine keeps its npm name; isothrum.com serves `web/pkg/isothrum_wasm.js` and
`isothrum_wasm_bg.wasm` from `public/wasm/`.

## Licence

Apache-2.0. See [LICENSE](LICENSE).
