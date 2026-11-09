# Changelog

All notable changes to Isothrum are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Isothrum is the runner (crate and CLI) and the lab-model compiler (drawing → spec)
plus its WASM build. It executes what Isoloom generates; the apps that embed it share it.

## [Unreleased]

### Fixed

- **An update restarts a running VM whose boot-time settings changed.** Vagrant applies a VM's
  synced folders, networks, forwarded ports, size and provider settings only when it boots it, and
  an update reran `up`'s steps on the running VM: a host folder added to a running `docker-vm`
  lab was never mounted in its VM, so its container saw an empty folder (the same for any such
  change to a running `vagrant` VM). The run's record now keeps each VM's boot digest (the
  manifest's `native.boot_digest`) at `up` and at every update; an update first restarts each
  running VM whose digest changed (`vagrant reload <define> --no-provision`, in the run's Vagrant
  state, as step `reload-<machine>`), then runs `up`'s steps, which provision it and start what
  runs in it again. A digest that isn't known on both sides (a run recorded before, a manifest
  without one) restarts nothing; the update records the manifest's.

- **A lab's own access machine reaches the lab through its router.** On Compose, `attach` started
  the access service alone (`--no-deps`), without the service that sets its routes (Isoloom's
  `<machine>-routes`, in its network namespace): an access machine on its own network reached
  nothing behind the router. Attach and detach now take the services in the machine's namespace
  (`network_mode: service:<machine>`, read from `docker compose config`) with it, and attach waits
  for them (`--wait`).
- **`attach` joins the chosen network by the name the run gave it, and says when it can't.** On
  Compose it guessed the network as `<project>_<network>` and swallowed every failure to join
  it, so an attach reported (and its event claimed) a network its machine wasn't on. The network
  must now be one of the manifest's (`unsupported` otherwise, before anything starts), is joined
  by its rendered native name (as status shows it), and a failure to join fails the attach
  (`tool_failed`); only a container already on it is left as it is. A Vagrant run refuses a
  chosen network (`unsupported`): a VM's networks are its Vagrantfile's, and the network was
  ignored.
- **An access machine whose namespace can't be read isn't started without its routes.** When
  `docker compose config` printed nothing the runner could read, attach and detach took the
  machine alone (no routes, or routes left running); they now fail (`tool_failed`).

- **A Compose config the runner can't read no longer passes for an empty one.** When `docker
  compose config --format json` printed something that isn't its JSON, `compose.check_ports_free`
  checked no port (so a taken one surfaced later as Docker's bind error), and
  `compose.remove_stale_networks` read every network of the project as stale and removed them,
  live ones included, during an update. Both steps now fail (`tool_failed`) and act on nothing,
  as attach and detach already did.

- **`docker compose ps` output the runner can't read no longer passes for no container.** When
  `ps --format json` printed something that isn't its JSON, every machine read absent: a resume
  of a parked run whose containers were there dropped its parked mark and failed with
  `nothing_to_resume`, `compose.pin_ports` re-picked the pins of existing containers, and the
  containers of a running `docker-vm` VM read absent. The run's status, park, resume and those
  steps now fail (`tool_failed`); in a VM, the read counts as a missed one (tried again, then the
  containers read `unknown`, "unreadable: …").

- **A target this host doesn't run is named as the manifest writes it.** Opening a run whose
  manifest leaves this host out said `target "docker" on this host isn't supported…`, the JSON
  quotes kept; it now says ``target `docker` on this host``.

- **A recorded run's Terraform state that doesn't parse fails the orphan sweep.**
  `Runner::orphans` skipped such a state silently, so the resources it holds, still live, were
  listed as orphans; it now fails with `io`, as it does for a state it can't read.

- **A Vagrant that can't list its plugins is said once.** When `vagrant plugin list` couldn't
  run or failed, `isothrum-machine`'s detection read no plugin as installed, so every provider
  that needs one said its plugin wasn't installed. Each now says that Vagrant couldn't say which
  plugins it has, with why.

- **The editor's errors say their cause.** A zip that couldn't be written said only "writing the
  zip", and the compiler's `PortName` and `Yaml` errors only what they were doing; their message
  (a `JsError`'s, for the editor) now ends with the cause.

- **A run's record names the manifest it was last brought up or updated from.** An update runs
  a regenerated manifest, but the record kept the one the run started from, which is what
  `isothrum reap` (and any operation that has only the record) reopens. `RunRecord::manifest` is
  now set at every `up` and update, with the VMs' boot digests.

### Added

- `Running::console_command(machine)`: the command that opens a console into any machine of the
  run, a pure query that reads the run's record as status does (a Vagrant run's console gets the
  host ports its start pinned). On a Terraform run: `ssh -t` to the machine's address as
  its user (`ssh_user_<machine>`, else `ssh_user`, else root), through the run's SSH settings.
  On Compose: `docker compose exec` of the service. On Vagrant: `vagrant ssh` of the define. A
  Compose service inside the run's VM (`docker-vm`, `cloud-docker`, Proxmox) goes into that VM,
  then into the service's container. Each starts bash where the machine has it, else sh.
- `Running::retire(machine, opts, events)` (and `isothrum retire`): takes a machine out of the
  running run ahead of its removal from the manifest, or its move out of the default start, and
  the update after: what that update wouldn't remove itself. A Vagrant VM is destroyed alone
  (`vagrant destroy -f <define>`: Vagrant forgets a define the Vagrantfile no longer has). A
  Compose service is stopped and removed with the services in its namespace (`docker compose
  [--profile access] rm -sf <service> <its sidecars>`: an update touches no service outside the
  default start, so one moved back to an on-demand machine would keep running); inside the run's
  VM (`docker-vm`, `cloud-docker`, Proxmox) its container and the containers in its namespace go,
  through the VM's way in. A machine a Terraform module makes is left to the update's apply. The
  VM a run's containers run in (its `guest`) is refused (`unsupported`: every container would go
  with it), as is a machine there is nothing to take out of (no native name, a Compose service
  with no VM to run in, an on-demand machine on a server or cloud). New operation in events:
  `retire`.
- `Running::update` (and `isothrum update`): brings a running run up to date with its manifest,
  regenerated since it started (a machine added to the lab or taken out). `up`'s steps run again
  with the params the run was started with, so each engine converges, then the guests' secrets,
  the ready conditions and the outputs, as at `up`. A machine that was running keeps what it was
  given: guest secrets go to new machines only. The run stays ready meanwhile; a sealed run stays
  sealed. New operation in events: `update`.
- **`isothrum console <machine>` and `isothrum retire <machine>`**: the CLI's way to
  `Running::console_command` (printed as one line to paste in a POSIX shell, or its JSON) and
  `Running::retire` (with `--param` and `--secret-from-env`, as `update`).
- `ShellCommand::sh_line()`: the command as one line a POSIX shell runs as it is, to paste in a
  terminal: `cd <its folder> && VAR=value… program args…`, each word that needs it single-quoted
  (a `sh -c` script with `;` or `>`, a JSON value), so the shell splits it back into the argv.
  `isothrum console` prints it; before, the CLI joined the words unquoted and left the folder
  out, so a pasted console broke.
- The CLI's `attach` and `detach` take `--param` and `--secret-from-env`.
- **The machine-setup backend holds UTM and its plugin to a version.** `isothrum-machine`'s
  provider detection reads each Vagrant plugin's version and UTM's own (its app bundle's
  `CFBundleShortVersionString`), and reports UTM unavailable with the way out
  when either is too old: `vagrant_utm` before 0.2.0 (it reads a VM's forwarded ports through a
  script UTM refuses, and has no `started` state) or UTM before 4.6.5 (the plugin refuses it).

### Changed

- **`ShellCommand::cwd` is a `PathBuf`**, no longer an `Option`: a console always runs in the
  run's folder.
- **`Running::attach(pick, opts, events)` and `Running::detach(machine, opts, events)` take the
  operation's options** (secrets and params an attach or detach that changes the run needs; pass
  `RunOptions::new()` for none), with the access machine's choice as an `AccessPick`, in the
  order `retire` and `provision` take theirs.
- **`isothrum-machine`'s `storage::load` returns `io::Result<Owned>`.** It read a record it
  couldn't read or parse as an empty one, so a caller that loads, changes and writes the record
  back forgot everything it held after one failed read. Only a missing file is an empty record
  now; a damaged one is an `InvalidData` error.
- **`RunnerConfig::new` honours the user's Docker context.** It started processes with
  `SystemProcesses::default()`, which never read the Docker CLI's config, so a runner built with
  it didn't drop `DOCKER_HOST` and `DOCKER_CONTEXT` for a context the user picked.
  `SystemProcesses::default()` now reads `$DOCKER_CONFIG/config.json`, else
  `~/.docker/config.json`; `SystemProcesses::new` and `SystemProcesses::for_host` are gone (use
  `SystemProcesses::default()`).

### Removed

- The run-manifest values Isoloom never writes are no longer read: a `static` output, an expiry in
  minutes from now, a dotenv state file and a WinRM guest delivery (Isoloom 1.0.10 removed them from
  the manifest).
- **`Running::shell_command`.** The access machine's shell is `Running::console_command` of its
  name (the `AttachReport`'s `machine`), which starts bash where the machine has it, else sh.
  `isothrum shell` goes with it: use `isothrum console <machine>`.

- **`isothrum-machine` keeps only provider detection and the download record's format.** Its
  public API is now `providers::{detect, Provider, ProviderStatus}` and `storage::{Owned,
  OwnedBox, OwnedImage, load, parse_box_list}`. Gone, with no user left: the self-tests
  (`selftest`, `BootProbe`, `ElevatedBoot`, `Vm`), the system report (`system`, `SystemReport`,
  `DockerDenied`), `Dirs`, `Error` and `Result`, the `exec` module, the record's writing half
  (`storage::{path, update, Before, image_ids, box_list, claim, tracked}`), and the provider
  helpers `parse_plugins`, `parse_plugin_versions`, `version_at_least`, `kvm_problem` and
  `HYPERV_NEEDS_ADMIN` (still used inside `detect`).
- **The `Hooks` port** (`Hooks`, `HookError`, `RunView`, `Runner::add_hooks`) and the
  `hook_failed` warning code (gone from the event schema): nothing implemented it. An embedder
  that keeps something beside a run does it around the operation's call.
- **`RunRecord::manifest_sha256` and `LoadedManifest::sha256`**: nothing read the hash. A record
  written with it still loads.
- **`Serialize` and `Deserialize` on `isothrum-machine`'s `Provider` and `ProviderStatus`**:
  nothing wrote or read either as JSON (an embedder reads the fields, and a provider's wire name
  is `Provider::id`).
- **`isothrum-machine`'s cloud providers.** `Provider::{Aws, Azure, Gcp, DigitalOcean, Linode,
  Oci}` and `Provider::is_cloud` go: detection never reports them (a cloud lab is Terraform's, not
  a Vagrant provider's).
- **`Compiled::yaml`.** `Compiled::into_parts` gives the spec's text, with the spec and the
  project's files.
- **`Runner::in_flight`, `process::capture` and `process::Captured`** are internal now: no
  embedder read them.
- **`RunnerConfig::with_processes` and `RunnerConfig::with_ports`.** Set `RunnerConfig::processes`
  and `RunnerConfig::ports` on `RunnerConfig::new(home)`, as every other field is set.
- **Runner API nothing used:** `Running::outputs` (read `Running::status`'s `outputs`),
  `RunnerConfig::with_clock`, `JsonLines` (an `Events` sink is a closure), `FixedClock` (now the
  crate's own test double), and `this_host` (now internal).
- **More API nothing used.** In `isothrum-runner`: the `Clock` port (`Clock`, `SystemClock`,
  `RunnerConfig::clock`: the runner reads the system's clock), `Recorder`, `EVENTS_VERSION` (an
  event's `v`), `Running::path`, and the test doubles' `FakePorts::take`, `FakePorts::answer`,
  `Reply::after` and `FakeProcesses::lines` (read `FakeProcesses::calls`), and `From<Option<i32>>`
  for `process::Exit` (use `Exit::new`). In
  `isothrum-compiler`: `Compiled::spec` (`Compiled::into_parts` gives the spec). In
  `isothrum-machine`: `Provider::ALL`, `Provider::is_remote`, and `ProviderStatus`'s `remote`,
  `hypervisor`, `plugin`, `plugin_installed`, `hypervisor_version` and `plugin_version` (its
  `reason` says what is missing or too old).
- **The ad-hoc attack box.** The runner no longer stands up an attacker box of its own when a lab
  declares no access machine: no `<run>-isoloom-access` container on the lab's network (on this
  computer's Docker, inside a `docker-vm` VM, inside a VM Terraform made on a server or a cloud),
  no `isoloom-access` VM added to a VM lab through Isoloom's Vagrantfile hook (gone from Isoloom's
  Vagrantfiles too), and status no longer probes for one; `down` and `park` no longer reap one.
  `attach` and `detach` on a run that declares no access machine now fail with `unsupported`
  ("this run declares no access machine"). A lab's own on-demand access machine
  (`access: true`: a Compose service in the `access` profile, a Vagrant define) is unchanged. An
  embedder adds a box of its own as an ordinary machine of the lab, with `Running::update`,
  `Running::retire` and `Running::console_command`.
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
