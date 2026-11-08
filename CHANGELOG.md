# Changelog

All notable changes to Isothrum are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Isothrum is the runner (crate and CLI) and the lab-model compiler (drawing → spec)
plus its WASM build. It executes what Isoloom generates; it is shared by the Isothrum
desktop app, the isothrum.com editor and the Cyber CTF launcher.

## [Unreleased]

### Added

- `Running::update` (and `isothrum update`): brings a running run up to date with its manifest,
  regenerated since it started (a machine added to the lab or taken out). `up`'s steps run again
  with the params the run was started with, so each engine converges, then the guests' secrets,
  the ready conditions and the outputs, as at `up`. The run stays ready meanwhile; a sealed run
  stays sealed. New operation in events: `update`.
- `Running::attach_with` and `detach_with` take the operation's options (secrets and params an
  attach or detach that changes the run needs), with the box choice as a `BoxPick`; the CLI's
  `attach` and `detach` take `--param` and `--secret-from-env`.
- **The attack box on a container lab in a VM Terraform made** (`cloud-docker` on every cloud,
  a container lab on a Proxmox server). `attach` with no access machine stands the ad-hoc box up
  as a container inside the run's guest VM, as the Vagrant driver does for `docker-vm`: `docker
  run -d --name <run>-isoloom-access --label isoloom.access=true --network <lab net> <image> sleep
  infinity`, the lab's other networks joined best effort, its address read with `docker inspect`;
  `detach` is `docker rm -f`; status reports it as the run's access machine (role access, its
  address on the lab network) while the VM runs; `shell_command` is `ssh -t` to the VM then `sudo
  docker exec -it`. The VM is reached as the guest's own `ssh.run` steps reach it (its
  `${outputs.ip}` from the run's state, the guest's user, the runner's key and the run's SSH
  configuration, through the run's jump and the step's `via`). The container logic is shared by
  both drivers (`driver::vm_box`), only the way into the VM differs.
  **Still unsupported:** a VM lab on a cloud or a server (`cloud-vm`, Proxmox VM labs) has no VM
  to put the box in and is refused (`unsupported`, "…on a run with no container host VM"), as is
  a manifest's on-demand access machine on a Terraform run.
- **The machine-setup backend holds UTM and its plugin to a version.** `isothrum-machine`'s
  provider detection reads each Vagrant plugin's version (`parse_plugin_versions`) and UTM's own
  (its app bundle's `CFBundleShortVersionString`), and reports UTM unavailable with the way out
  when either is too old: `vagrant_utm` before 0.2.0 (it reads a VM's forwarded ports through a
  script UTM refuses, and has no `started` state) or UTM before 4.6.5 (the plugin refuses it).
  `ProviderStatus` carries both versions (`hypervisorVersion`, `pluginVersion`). What the Cyber
  CTF launcher checks on its own, now shared.

### Changed

- **The attack box answers to `attackbox`.** Every ad-hoc attack box the runner starts gets that
  hostname: the container beside a container lab here, the container inside a lab's VM (here, on
  a cloud account, on Proxmox), and the VM added to a VM lab (through Isoloom's hook `hostname:`,
  Isoloom 1.0.10; an older Vagrantfile keeps its define name). A learner's prompt reads
  `root@attackbox` instead of a Docker container ID or Isoloom's internal `isoloom-access`, which
  stays the name status reports.
- **A VM lab's ad-hoc attack VM runs the learner's container image.** `attach`'s `image` on a plain
  VM lab (no `guest`, no access machine of its own) no longer becomes the attack VM's Vagrant box:
  the Cyber CTF launcher's `cyberctf/attack-box@sha256:…` was asked of Vagrant Cloud and answered
  404. The definition now passes it to Isoloom's hook as `image:` (Isoloom 1.0.10), which makes
  the VM a container run as a VM: Debian 12 with Docker, the image idle on the VM's network, so
  the VM's lab address is the attacker's; the shell into it is `vagrant ssh isoloom-access` then
  `docker exec`. No image gives a plain VM of the host's default box, as before; a Vagrantfile
  from before the hook's `image:` is refused with the way out (regenerate the lab). The VM's box
  is still `ISOLOOM_ACCESS_BOX` when set.
- An attach report names an ad-hoc box `isoloom-access`, as status does; its container name is
  in `native`.

### Fixed

- **`shell_command` reads the run's record**, as status does: the shell into an attack box goes
  through the run's SSH jump, and a Vagrant run's shell gets the host ports its start pinned.

### Removed

- **The desktop app's crate, `apps/desktop`.** The Isothrum desktop app lives in its own
  repository, `isoloom/isothrum-desktop`, which ships it (installers, the Microsoft Store) on the
  crates published from here; the copy kept here "while that move completes" had drifted from it
  and had no release path. Its CI jobs, the installer workflow and its place in the xtask
  dependency rule go with it. The 2026-11-08 fixes made on that copy (the editor's command names,
  the hypervisor choice, UTM's state) are in the desktop repository.

## [1.0.5] — 2026-11-08

### Added

- **A VM lab gets its attacker box as a VM added to the running environment.** On a Vagrant run
  with no `guest` and no access machine of its own (a plain VM lab, one VM per machine), `attach`
  now adds a node to the live lab as GNS3 adds one to a topology: it writes a one-line definition
  under the run's state folder (`<VAGRANT_DOTFILE_PATH>/adhoc/isoloom-access.rb`, which every
  Vagrantfile Isoloom 1.0.9 writes loads at the end of `Vagrant.configure` through its
  `IsoloomAdHoc` hook), naming the chosen lab network (the first otherwise), a free address among
  the manifest's machines (`.252` in a /24 with no tools) and the learner's box (none: the
  Vagrantfile's default for this host's CPU, Kali on x86, the Debian cloud image on Arm), then
  boots it alone with `vagrant up isoloom-access --provider=<p>`. Status reports it as the run's
  access machine at that address, `shell` is `vagrant ssh isoloom-access`, `detach` destroys it
  alone and removes the definition with the lab left up, and `down` takes it with the lab. On
  VirtualBox, VMware, Parallels, libvirt and QEMU; Hyper-V, ESXi and UTM refuse it (declare an
  `access: true` machine there), as a Vagrantfile written before the hook does. Before, a VM lab
  refused every ad-hoc box ("hot-adding an ad-hoc access VM … isn't supported").

- **A container lab in a VM gets an ad-hoc attacker box inside the VM.** On a `docker-vm` run
  (the manifest's `guest`), the Vagrant driver's `attach` now stands the attacker box up as a
  container inside the helper VM, on the lab's Docker network there (an idle container from the
  learner's image, Kali by default, named `<run>-isoloom-access`, plugged into the lab's other
  networks best effort), through `vagrant ssh <vm> -c "docker …"` with the run's state folder;
  `detach` removes it, status reports it as the run's access machine with its address on each
  lab network, and `shell` is `vagrant ssh <vm> -c "docker exec -it …"`. Before, the driver
  refused every ad-hoc box ("hot-adding an ad-hoc access VM … isn't supported"). A VM lab with no
  on-demand access machine still refuses one: it has no Docker to put a container in.

### Changed

- **A `ShellCommand` carries the variables it needs (`env`).** A shell into a Vagrant run's box
  (`vagrant ssh …`) only finds the VM with the run's state folder (`VAGRANT_DOTFILE_PATH`) and
  its forwarded ports (`ISOLOOM_HOSTPORTS`), which the embedder's terminal never had: the command
  now names them, the CLI prints them before the argv, and a Compose run's command has none.

### Fixed

- **A container read in a VM that misses once is read again.** The way into a `docker-vm` run's
  VM (`vagrant ssh`, which on some providers reads the VM's SSH port from the hypervisor first)
  can miss once while the VM runs; that one miss made the lab read as not running, and refused an
  attach on it. The read is tried once more after a second before the containers count as
  unreadable.
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
