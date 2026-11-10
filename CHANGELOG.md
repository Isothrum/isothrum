# Changelog

All notable changes to Isothrum are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Isothrum is the runner (crate and CLI) and the topology compiler (drawing → spec)
plus its WASM build. It executes what Isoloom generates; the apps that embed it share it.

## [Unreleased]

### Highlights

The next release is a **major** one: it removes and changes public API (every such entry below
starts with **Breaking:**). [docs/migrating-to-2.0.md](docs/migrating-to-2.0.md) says what to
change.

- **Breaking, for embedders:** public API nothing used is gone (the `Hooks` port, most of
  `isothrum-machine`, test-double and builder leftovers), the process port lives at the crate root,
  and `park`, `attach` and `detach` take the operation's options.
- **Breaking, for the command line:** `isothrum shell` becomes `isothrum console <machine>`,
  `--ssh-jump` is taken by `up` only, and `attach` no longer stands up an ad-hoc box.
- **Breaking, for event readers:** an event's `run` and `op` can be absent (an `error` outside an
  operation), and the `hook_failed` warning is gone.
- **Breaking, for every run:** Isothrum reads run manifest version 1, the frozen contract the next
  Isoloom release writes, and refuses the version 0 that 1.x read: regenerate the files first.
- **New operations:** `update` (bring a running run up to date with its regenerated manifest),
  `retire` and `console`; `isothrum completions`.
- **CI-ready command line:** Ctrl-C and SIGTERM stop the tools and exit 130/143 with a `cancelled`
  error; with `--json` every failure is an event; a step's timeout counts time without progress.
- **Corporate environments:** proxy and CA settings reach an elevated Hyper-V session and a
  containerised Terraform; an owner-only home on Windows; the user's `TF_PLUGIN_CACHE_DIR` is kept.
- **Releases you can verify:** signed binaries with SBOM and provenance, `cargo binstall`, CI on
  every pull request, and documentation organised as tutorial, how-tos, reference and explanation.
- **Many fixes** where unreadable tool output passed for success (Compose `config` and `ps`,
  `reap`), and a sensitive output masked in `status`.

### Fixed

- **`project.archive` no longer puts a Terraform state or Isoloom's other outputs in the guest's
  copy.** The archive left out only `.git`, `.vagrant`, `.terraform` and the top's
  `.isoloom-project*`, plus what `.isoloomignore` listed, so a `*.tfstate` (the secrets Terraform
  saw, in plain text), its backups and lock, crash logs, the runner's `.isoloom-reach/` logins and
  every `.isoloom/` output went into the archive a step pipes into a guest. It now leaves out
  exactly what the generated files' copies do (`isoloom_manifest::ignore::Rules::for_copy`):
  Isoloom's outputs but those the step's new `with.outputs` names, and, at any depth, version
  control and the tools' state, in those outputs too. The walk never enters a folder no copy
  takes.
- **Ctrl-C or a cancelled CI job no longer leaves the tools running.** Each tool runs in its own
  process group (a job object on Windows), so SIGINT and SIGTERM reached only `isothrum`, and
  Terraform or Vagrant kept running. `isothrum` now races its command against SIGINT and SIGTERM
  (Ctrl-C, Ctrl-Break, the console closed, a log-off or a shutdown on Windows): the operation is
  dropped, which stops each tool's process tree, and the command exits 130 (SIGINT) or 143
  (SIGTERM) once they have stopped.
- **A tool cut short is asked to stop before it is ended.** A cancel, Ctrl-C or a step's timeout
  sent SIGKILL to the tool's process group, so a Terraform cut short lost the state it had not
  saved yet. On Unix the group now gets SIGINT (Terraform, Vagrant and Compose stop cleanly on
  it), and SIGKILL once the tool has exited or 30 seconds later. `isothrum` waits for them
  before it exits ("Waiting for the tools to stop"); a second signal ends them at once. An
  embedder waits with the new `SystemProcesses::stopped` (`is_stopping`, `end_stopping`). On
  Windows a tool, headless, is still ended at once.
- **An operation cut short says so.** Dropping an operation's future (an embedder's cancel, the
  command line's Ctrl-C) ended its events mid-stream and left the run `starting` (or `stopping`,
  …), so the next `up` said it was already running. Its events now end with an `error` whose code
  is the new `cancelled`, after the `state` move to `failed`; the record says `failed`
  (`last.code: cancelled`), so the next `up` clears what it left.
- **The elevated Hyper-V session no longer reads an earlier session's files.** The run's
  `elevated/` folder kept the `ready`, `exit-<n>` and `out-<n>.log` of the operation before, which
  the next one read as its own (a broker taken for started, a command for done). A session now
  clears `elevated/` and works in its own `elevated/<token>/`; the runner deletes `out-<n>.log`
  and `exit-<n>` once read, and the session's folder when it ends; `exit-<n>` appears whole
  (renamed into place).
- **An elevated Hyper-V session no longer ends six hours in, and a broker that stops fails its
  request at once.** The broker ended itself six hours after its prompt, whatever the operation
  still had to run, and a request whose broker had stopped waited for its answer until its step's
  limit, then read as a `timeout`. A session now lasts as long as its operation; a broker that
  stops other than at the session's end writes why in `ended`, and the request in flight fails
  with `elevation_failed` saying so, as does every later call of the operation. A request whose
  file couldn't be written, or whose answer couldn't be read, now fails the session too (the
  broker waited for it and took no later request), and a failed `whoami /groups` is traced.
- **The macOS WinRM proxy passes a client's close on to the guest.** It shut `nc`'s input down,
  which leaves a pipe open, so the guest never saw the client's end: each WinRM connection's
  `nc`, its connection to the guest and the proxy's task for it stayed until the operation ended.
  It closes `nc`'s input now, as the SSH relay already did.
- **SSH through a jump host works from a home whose path has a space.** `ssh` was given
  `-F <home>/runs/<run>/ssh_config`, and `-J` hands that path to each jump host's own `ssh`
  through a shell, unquoted: under macOS's `~/Library/Application Support` every jump failed at
  once ("Can't open user config file …/Library/Application"), so a `ready_file` read through a
  Proxmox run's router never answered. `ssh` now runs in the run's state folder with
  `-F ssh_config`; a console over SSH (`Running::console_command`) runs there too.
- **A `ready_file` wait says why its reads fail.** A read over SSH that failed was dropped
  without a word, and a host that never answered failed with "didn't answer over SSH within
  600s" alone. Each new reason is now a `ready` event (`waiting`, "no answer over SSH yet: …",
  ssh's last error line and its exit status, masked), and the wait's error ends with the last
  one, out of time included.
- **Two starts in one home no longer take the same slot.** Two `up`s at once (two processes)
  both read the records before either recorded its slot. A start now picks its slot and records it
  under an OS lock on `<home>/slots.lock`, waiting up to 30 s for another start's pick, then
  failing with `locked` (`Error::SlotsLocked`).
- **Terraform uses the user's `TF_PLUGIN_CACHE_DIR`.** The runner replaced it with its own cache
  under the home. A cache the user set is now used as it is (neither created nor narrowed); without
  one, the runs share `<home>/terraform-plugins` as before.
- **The macOS ESXi relay keeps the user's `no_proxy`.** It replaced it with the loopback alone,
  so an internal host the user reached directly went to their proxy; it is now the user's
  `no_proxy` (else `NO_PROXY`) plus `localhost,127.0.0.1`. `http_proxy` is still the relay's (WinRM
  reaches the guests only through it); `https_proxy` is left as it is.
- **`--name`'s help no longer defines it by itself.** It now says the default is the environment's
  name in the manifest.

- **The checks no longer run on a setup machine that is still powering itself off.** Waking the
  controller for the checks asked it over SSH whether it was halting, and took a failed SSH (a
  controller already on its way down) for "not halting": it skipped the boot and the checks then
  found it off. The controller now answers in words; no answer is waited out like a halting one,
  then it is booted. The wait is bounded: each ask may go 20 s without an answer, and a
  controller still running without saying it stays up after three minutes fails the step
  (`tool_failed`) rather than be asked for up to two hours.
- **An update restarts a running VM whose boot-time settings changed.** Vagrant applies a VM's
  synced folders, networks, forwarded ports, size and provider settings only when it boots it, and
  an update reran `up`'s steps on the running VM: a host folder added to a running `docker-vm`
  run was never mounted in its VM, so its container saw an empty folder (the same for any such
  change to a running `vagrant` VM). The run's record now keeps each VM's boot digest (the
  manifest's `native.boot_digest`) at `up` and at every update; an update first restarts each
  running VM whose digest changed (`vagrant reload <define> --no-provision`, in the run's Vagrant
  state, as step `reload-<machine>`), then runs `up`'s steps, which provision it and start what
  runs in it again. A digest that isn't known on both sides (a run recorded before, a manifest
  without one) restarts nothing; the update records the manifest's.

- **A run's own access machine reaches the run through its router.** On Compose, `attach` started
  the access service alone (`--no-deps`), without the service that sets its routes (Isoloom's
  `isoloom-routes-<machine>`, in its network namespace): an access machine on its own network
  reached nothing behind the router. Attach and detach now take the services in the machine's
  namespace (`network_mode: service:<machine>`, read from `docker compose config`) with it, and
  attach waits for them (`--wait`).
- **`attach` joins the chosen network by the name the run gave it, and says when it can't.** On
  Compose it guessed the network as `<project>_<network>` and swallowed every failure to join
  it, so an attach reported (and its event claimed) a network its machine wasn't on. The network
  must now be one of the manifest's (`network_unknown` otherwise, before anything starts), is
  joined by its rendered native name (as status shows it), and a failure to join fails the attach
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
- **A fresh Compose run no longer starts on ephemeral ports when its ports can't be pinned.**
  `compose.pin_ports` wrote no pins and passed when `docker compose config` printed something
  that isn't its JSON, or when no free host port was left for an ephemeral publish: the run
  started on ports Docker picked, and a resume got new addresses. The step now fails
  (`tool_failed`), naming the port that has no free host port left.
- **`isothrum reap` no longer passes when it couldn't query the cloud.** A query of what the
  cloud holds that couldn't run or be read was listed (`not checked: …`) but the command passed,
  as for a clean sweep; it now fails.
- **`isothrum reap` says the sweep when the cloud can't be searched after it.** When the search
  for leftovers failed (a recorded run's record or Terraform state unreadable, `io`) after the
  destroys, the command printed nothing of the sweep; and the failure was taken for said when the
  last destroy had failed (nothing printed at all), or stamped with the destroyed run's `run` and
  `op`. It now prints the sweep (with `--json`, its `result`, `orphans` and `unread` null), then
  the failure (an `error` event without `run` nor `op`), and exits 1.
- **A guest delivery's input file that can't be removed fails the delivery.** The values a
  `guest` delivery sends pass through an owner-only `guest-<machine>.in` in the run's state
  folder; a failure to remove it once delivered was only traced, leaving the values there. It now
  fails the step (`io`). The README and the crate docs list the four deliveries (`env`, `tf_var`,
  `state_file`, `guest`) and no longer say no value is ever stored: they name the two owner-only
  files, and no longer describe a `dotenv` state file.
- **`isothrum` says why a run folder's `.isoloom` can't be read.** Any failure to list it (a
  permission refused, a file in its place) read as "no run manifest: run `isoloom generate`";
  only a missing folder does now, and any other failure is reported as it is.
- **An elevated Hyper-V call with a standard input is refused, not run without it.** The
  elevated broker starts each program with no input, so a call that pipes a file in ran with
  none; it now fails with `ProcessError::Refused` before anything runs.

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

- **A sensitive output is masked in a run's status.** Events and results showed it as `***`,
  but `Status` and `Started` held the value itself in `OutputValue::value`, and `isothrum status
  --json` printed it. `OutputValue::value` is now `***` for a sensitive output (in its JSON and its
  `Debug` too).
- **An output whose `format` can't be filled is said.** It quietly became an output with no
  value; `up`, `resume` and `update` now emit an `output_unformatted` warning (new in the event
  schema) saying which format named what the run doesn't have, before the outputs.
- **An operation that failed ends with its own error when the run's record can't be updated.**
  The record's I/O error replaced the operation's, and the closing `state` event was never
  emitted. The operation now ends with its own error, after a `record_not_saved` warning (new in
  the event schema) and its closing `state` event.

- **The compiler addresses a LAN whose block doesn't start its last octet.** It took a block's
  usable addresses from its size alone (.2 on, .10 on a /24), so a machine on `10.10.1.8/29` or
  `10.10.1.128/25` got `.2`, outside the block, and Isoloom refused the spec. The addresses are now
  Isoloom's own (`Cidr::host`): `.10` to `.12` on `10.10.1.8/29`, `.130` on `10.10.1.128/25`.
  A block Isoloom gives machines no address on (not a network's own address, larger than a /24 or
  smaller than a /29) is now the compiler's `BadCidr`, which says so; it was written into a spec
  Isoloom then refused.
- **The compiler refuses an address no machine can take on its LAN.** A machine's asked-for
  address was written into the spec as it was, so `.200` (or the gateway's `.9`, the router's
  `.14`, …) on `10.10.1.8/29` compiled, and Isoloom refused the spec. It is now the compiler's new
  `Error::AddressOutside`, naming the LAN, the machine, the address and the block. A /24's
  addresses below `.10`, which the compiler gives no machine itself, can still be asked for.

- **A cloud target's manifest is found from the run's folder.** Isoloom writes one manifest per
  cloud (`.isoloom/cloud-vm/aws/run.json`), but the command line looked only for
  `.isoloom/<target>/run.json`: `isothrum up <folder>` said nothing was generated, and `--target
  cloud-vm` that it didn't exist. `--target cloud-vm/aws` now names the AWS one, `--target
  cloud-vm` the only cloud generated, and no `--target` the only manifest of any target.

- **A guest delivery cut short leaves no secret on disk.** The values for a machine wait in an
  owner-only `guest-<machine>.in` in the run's state folder while SSH is retried (up to 15
  minutes); an operation cut short then (a cancel, Ctrl-C) skipped its removal, and the file
  stayed. It is now removed however the delivery ends.

- **An event log cut mid-character no longer stops the run.** The run's `events.jsonl` was read
  as UTF-8 text, so a log cut short inside a character (a full disk, a killed process) failed
  every later operation of the run with `io`, and `isothrum events` with it. Such a line now
  counts as one the runner can't read (`EventLog::unreadable`), and each event is written in one
  write, its line break included.
- **A run's event log is trimmed only by the operation holding the run's lock.** Each operation
  trimmed a long `events.jsonl` (over 4000 lines) before it locked the run, by writing the
  newer half to a new file renamed over it: a second operation on a running run replaced the log
  under the first, whose later events went to the file it replaced (on Windows the rename over
  the first's open log could fail the second with `io` rather than `locked`). An operation now
  opens the log to append before the lock, so a `locked` error is still logged, and trims it once
  the run is locked.

- **The run's SSH configuration reads the same to every OpenSSH.** It escaped a path's spaces
  with backslashes, which OpenSSH reads as escapes only from 8.7: Windows 10's 8.1, RHEL 8's 8.0
  and Ubuntu 20.04's 8.2 split a key under a home whose path has a space (`C:\Users\First Last`).
  Each path is now written between double quotes (with `/` on Windows, so a network home's `\\`
  stays whole); a home whose path no release reads as written fails the SSH call with `io`. The
  refused login's message names `:`, which a login takes.

- **A switch named like a cable's network is refused.** A switch called `link-<n>` nested its
  VLANs under the network of that name, which a cable then took over: the VLANs vanished from the
  spec. It is now refused as a LAN of that name is (`Error::ReservedName`).

- **A Windows starter opens each port on its transport.** The PowerShell starter took a port's
  number alone: a UDP port got a TCP firewall rule, a number drawn for TCP and UDP got the same
  rule twice, and a machine with only 443 was told IIS was for port 80. Each rule now has its
  port's transport (`project-53-udp`), and IIS's line names the web ports drawn.

- **Provider detection without Vagrant probes nothing.** `providers::detect` ran every
  hypervisor's probe (VirtualBox's liveness check takes up to 8 s) even when told Vagrant isn't
  installed, though each provider then said only that; it now answers at once.

- **A containerised Terraform gets the network settings the runner read.** It was handed the
  proxy, CA bundle and `TF_CLI_CONFIG_FILE` variables by name, chosen from the environment read
  when the runner was configured, but `docker run -e NAME` took their values from the process's
  environment of the moment; `docker` is now given those values.

- **`attach --json` and `reap --json` print events only.** Each ended with a JSON object that
  wasn't an event, on the stream documented as one event per line, and `attach`'s showed its
  image unmasked when it was a secret's value. `attach` now says where the machine is in its
  `result` event's `data` (masked as every event is); `reap` ends with a `result` event, no
  `run` nor `op`, whose `data` is the sweep.

- **A wait shows in text as it goes.** The `ready` events that say what a wait is waiting for
  (a `ready_file`'s "running: …", "no answer over SSH yet: …") reached `--json` only; the text
  output now shows them (`waiting: <condition>: <detail>`). A `tcp` condition emitted an empty
  `waiting` event every 2 s; it says once what it waits for.

- **A mistyped `--param` on an existing run is refused.** `down`, `park`, `check`, `update` and
  the other operations on a run that exists dropped a param the manifest doesn't take and passed
  a value outside its `values` unchecked. They now check what is given as a start does (an
  unknown name, a value outside `values`, a param the runner sets), then replay the recorded
  params.

- **A Terraform preflight says what is wrong with the module it checks.** With a
  `terraform.copy_module` step, a `with.from` that couldn't be filled, a folder outside the
  project or one that couldn't be read all read as "the Terraform module … has no `.tf` file:
  generate the run's files again", naming the generated module beside the manifest. The fill
  error, `<step>.with.from: … isn't a folder inside the run's project` or the I/O error is
  reported now, and a module without a Terraform file is named where it is.

- **The setup machine's boot and halt take the step's limit on the run's provider.** Vagrant's
  `wake_controller` and `park_controller` read the step's `timeout_s` only, never its
  `provider_timeout_s`, so a longer limit for ESXi never reached them.

- **A `down` forgets the run's pinned SSH host keys whatever the driver.** Only a Terraform
  run's `down` removed `known_hosts` and `ssh_config` from the run's state folder, so a Compose or
  Vagrant run that reaches a machine over SSH refused its recreated machine's new host key at the
  same address.

- **Adopting a Terraform state never leaves it in both folders.** Any failed rename fell back to
  a copy and a removal, so a state whose folder couldn't be written was copied into the run and
  stayed where it was. The copy is now the fallback across file systems only, and a copy whose
  source can't be removed is removed itself.

- **An `ESXI_HOSTPORT` that isn't a port is a `param_invalid` error.** It became 22, and the
  macOS relays pointed at the wrong port.

- **Errors that read as something else are said.** A folder entry that couldn't be read (the
  module copy, the runs' list, the run folders the CLI finds) was skipped; an elevated answer that
  wasn't an exit code read as a program killed by a signal; a public SSH key that couldn't be
  read was made again by `ssh-keygen`, which failed on its overwrite prompt. Each is now an error
  naming its cause. A VM address that `status` can't read (on ESXi) and a guest `host` that can't
  be filled are still left out, and traced with why.

### Added

- **Release binaries.** A tag publishes `isothrum` for Linux (x86_64, aarch64; static musl),
  macOS (arm64, x86_64) and Windows (x86_64) on GitHub releases, each archive with the licences of
  every crate it is built from, plus a CycloneDX SBOM, `SHA256SUMS`, a keyless cosign signature of
  it and build provenance attestations (`docs/how-to/verify-a-release.md`).
  `cargo binstall isothrum-cli` installs them.
- **CI on every push to `main` and every pull request**: clippy and the tests on Linux, macOS and Windows, the
  docs, the quality ratchet, `cargo deny`, `cargo machete`, typos, `cargo semver-checks`, the
  MSRV and the WebAssembly build. Every workflow's actions are pinned to commits; Dependabot
  proposes updates.
- **Documentation** organised by purpose (`docs/README.md`): a tutorial, how-tos (CI, proxies and
  CAs, Hyper-V without admin rights, moving the home, offline use, cost controls, SSH keys), the
  state layout, the event stream, the supported tool versions, the architecture and the security
  model; the policies (what is stable, run manifest versions, MSRV, supported releases); a README
  per crate; SECURITY.md, CONTRIBUTING.md, SUPPORT.md and the Code of Conduct.

- **With `--json`, every failure is an `error` event.** A failure before any operation began (a
  folder with no manifest, a value missing, an option clap refuses) was printed as text on
  standard error; it is now an `error` event on standard output (code `usage` for a command line
  that makes no sense), so a CI job reads every outcome from one stream.
- **`docs/reference/`**: `cli.md` (every command and option, written from the clap grammar),
  `exit-codes.md` (the exit codes, and every error code with its cause and remedy) and
  `environment.md` (every variable isothrum reads or sets for its tools). Tests fail when the
  first two drift from the code (`ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli
  reference` writes them).
- **`crates/isothrum-runner/schema/status.schema.json`**, the JSON Schema of `isothrum status
  --json` (`isothrum_runner::status_schema()`), committed beside the event schema and checked for
  drift the same way (`cargo run -p xtask -- schema` writes both).
- **`isothrum completions <shell>`** prints a completion script (bash, zsh, fish, PowerShell,
  elvish); it needs no home.
- **`ISOTHRUM_LOG` takes filter directives** (`isothrum_runner=debug,warn`), not only a level, and
  **`ISOTHRUM_LOG_FORMAT=json`** writes one JSON object per trace line.
- **Error codes `cancelled` and `usage`** (`ErrorCode::Cancelled`, `ErrorCode::Usage`).
- **`Event::new(seq, run, op, kind)`**: an event an embedder writes itself (a failure no operation
  reports), timed now.
- **Corporate networks.** The proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, either
  case) and the CA bundles (`SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`, `CURL_CA_BUNDLE`) reach the tools
  of an elevated Hyper-V session; a containerised Terraform (macOS, Proxmox) gets them and
  `TF_CLI_CONFIG_FILE` by name, with the files they name mounted read-only.
- **An owner-only home on Windows.** A home outside the user's profile gets a protected DACL (the
  account, SYSTEM, Administrators) that everything under it inherits, set with `icacls`; a home in
  the profile keeps the profile's.

- `ProcessSpec::env(name)`: the value a process is given for a variable.
- `AccessPick::{with_machine, with_network, with_image}`: an `AccessPick` is built from
  `AccessPick::default()` with them.
- `Running::console_command(machine)`: the command that opens a console into any machine of the
  run, a pure query that reads the run's record as status does (a Vagrant run's console gets the
  host ports its start pinned). On a Terraform run: `ssh -t` to the machine's address as
  its user (`ssh_user_<machine>`, else `ssh_user`, else root), through the run's SSH settings.
  On Compose: `docker compose exec` of the service. On Vagrant: `vagrant ssh` of the define. A
  Compose service inside the run's VM (`docker-vm`, `cloud-docker`, Proxmox) goes into that VM,
  then into the service's container. A Terraform machine's console and a Compose service's
  start bash where the machine has it, else sh; a Vagrant VM's is `vagrant ssh`'s, the VM's
  login shell.
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
  regenerated since it started (a machine added to the run or taken out). `up`'s steps run again
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
- `isothrum_runner::version_in` (the version a tool prints) and
  `isothrum_runner::vagrant_plugins` (each plugin of `vagrant plugin list --machine-readable`,
  with its version): the one reading of each, which the runner and `isothrum-machine` share.
  `isothrum-machine` now reads `vagrant plugin list --machine-readable`, not the human list.

### Changed

- **Breaking:** the machines of the environment a run manifest describes (not its router, jobs
  or checks) have the role `member` (was `target`), as Isoloom 2.0 writes them; status and
  readiness count `member` machines.

- **No more "lab" on the public surface.** The command line's help (`attach`, `detach`,
  `retire`), the runner's rustdoc and the crates' keywords speak of runs; the real-run test reads
  its run folder from `ISOTHRUM_REAL_RUN_DIR` (was `ISOTHRUM_REAL_LAB`); the tests' access machine
  is a neutral `user`.
- **Breaking: `Runner::events` returns an `EventLog`**: its `events`, and `unreadable`, how many
  lines of the log it couldn't read as an event. Those lines were dropped without a word, so a
  1.x log's `hook_failed` warnings vanished from `isothrum events`, which now says how many lines
  it couldn't read, on standard error.
- **Breaking: what was wrong with a call or a manifest value has its own error code, not
  `unsupported`.** `unsupported` meant both "this runner doesn't do it" and a mistake of the
  caller's or the manifest's. Now an `attach` (or `detach`) that names none of the run's several
  on-demand machines fails with the new `machine_ambiguous` (`Error::MachineAmbiguous`), one
  whose network the run doesn't have with the new `network_unknown` (`Error::NetworkUnknown`),
  and a manifest value the runner can't use as written (a step's `verdict` that isn't a regular
  expression, an SSH login, an `ssh.run`'s `wait_s`, `host` or `command`, a `project.archive`
  without its folder, a `terraform.copy_module`'s `from` or `when`) with `manifest_invalid`
  (`Error::Malformed`, naming the field). `unsupported`'s message no longer says "by this runner
  yet": much of what it refuses is a target's limit, not a missing feature. Also
  `manifest_invalid` now: a placeholder with no value in the operation (`Error::Unbound`, was
  `unsupported`), a `tcp` condition whose port isn't a number (was `not_ready`), a guest
  delivery to a machine whose address no output gives (was `unsupported`) and a Terraform module
  with no `.tf` file (was `io`).
- **Breaking: `elevation_declined` is `elevation_failed`** (`ErrorCode::ElevationFailed`). The
  code covered more than a declined prompt: an elevated session that didn't start, or whose
  broker stopped before the operation ended. The message says which.
- **The runner reads its environment once, in `RunnerConfig::new`.** The proxies, CA bundles,
  `TF_CLI_CONFIG_FILE`, Vagrant's variables and `USERPROFILE` it hands to an elevated Hyper-V
  session, a containerised tool and the macOS relays were read from the process's environment
  in the middle of each operation; they are read with `PATH` and `TF_PLUGIN_CACHE_DIR` when the
  configuration is made, so an embedder that changes its environment later builds a new one. An
  elevated session gets `RunnerConfig::path` as its `PATH`.
- **Breaking: Isothrum reads run manifest version 1.** Isoloom froze its run manifest at
  `manifest_version` 1, with a compatibility policy (`isoloom_manifest::COMPATIBILITY`: changes
  within a version are additive; a reader refuses what it doesn't know) and a schema published at
  `https://raw.githubusercontent.com/isoloom/isoloom/main/schemas/run-manifest-v1.schema.json`.
  Isothrum reads version 1 only: a `run.json` of version 0 (what Isoloom 1.x writes) or 2 fails
  with `manifest_version` before anything runs. Regenerate the files with the matching Isoloom; a
  run 1.x brought up is taken down with 1.x, or its files are regenerated in the same folder.
  Version 1's `timeout_s` and `provider_timeout_s` mean time without progress, as Isothrum already
  counts them (below). [Policies](docs/policies.md#run-manifest-versions) pairs each release with
  the version it reads.
- **Breaking: an event's `run` and `op` are optional.** `Event::run` is an `Option<String>` and
  `Event::op` an `Option<Op>`, absent from the JSON only on an `error` written outside an
  operation (a command line's usage error, a `status` that failed); the event schema no longer
  requires them. Every event an operation emits has both.
- **The exit codes 130 and 143.** A command cancelled by SIGINT exits 130, by SIGTERM
  143 (it was killed by the signal before). 0, 1 and 2 are unchanged.
- **Breaking: an elevated session's files.** A request's variables are no longer in its
  `req-<n>.json`: they are in `env-<n>.json`, which the broker reads and deletes before it starts
  the program, and each request carries the session's random token and its number, without which
  the broker deletes it unrun. Only a broker of this release reads them.

- **Breaking:** **A step's timeout counts time without progress, not its total time.** A docker-vm run's
  `docker compose up --pull missing` pulling a 1.1 GB image through a slow VM network was still
  downloading when its 30-minute step was stopped. A step's `timeout_s` (and every limit the
  runner gives its own tool calls) is now the time it may go without progress: a new output line,
  on either stream, that isn't blank and isn't one of the last 64 distinct lines it printed. A
  stuck pull repeating `36b0bafd7f97 Downloading 1.121GB` (or several layers taking turns) makes
  none and is stopped at its limit; a step that prints nothing is stopped at its limit from its
  start, as before. A step that keeps printing new lines stops at a ceiling, six hours after its
  start (or its own limit, when that is longer): no step stops sooner than before.
  `Error::Timeout` now holds a `TimedOut` (`NoProgress { after_s, last_line }` or
  `Ceiling { after_s }`) in place of `after_s`, its message says which (``step `up` showed no
  progress for 1800s and was stopped; its last line: `…` ``), and the `error` event carries it as
  `timeout` (`{"kind": "no_progress", "after_s": 1800, "last_line": "…"}`, or
  `{"kind": "ceiling", "after_s": 21600}`), secrets masked.

- **Breaking:** **`ShellCommand::cwd` is a `PathBuf`**, no longer an `Option`: a console always runs in the
  run's folder.
- **Breaking:** **`Running::attach(pick, opts, events)` and `Running::detach(machine, opts, events)` take the
  operation's options** (secrets and params an attach or detach that changes the run needs; pass
  `RunOptions::new()` for none), with the access machine's choice as an `AccessPick`, in the
  order `retire` and `provision` take theirs.
- **Breaking:** **`Running::park(mode, opts, events)` takes the operation's options**, as `down` and `retire`
  do (pass `RunOptions::new()` for none). It ran on empty options: a run recorded under another
  host label was parked anyway (now `running_elsewhere`), and its steps had no params given.
  `isothrum park` takes `--param`, `--secret-from-env`, `--host-label` and `--provider`, as
  `down` does. Its SSH goes through the jump the run recorded at `up`, as every later
  operation's does.
- **Breaking:** **`isothrum` takes `--ssh-jump` on `up` only.** Every command that runs an operation accepted
  it, but only an `up` that starts the run reads it (it is recorded; later operations go the
  same way): given anywhere else it did nothing. `RunOptions::ssh_jump` says so.
- **Breaking:** **`RunnerConfig::new` honours the user's Docker context.** It started processes with
  `SystemProcesses::default()`, which never read the Docker CLI's config, so a runner built with
  it didn't drop `DOCKER_HOST` and `DOCKER_CONTEXT` for a context the user picked.
  `SystemProcesses::default()` now reads `$DOCKER_CONFIG/config.json`, else
  `~/.docker/config.json`; `SystemProcesses::new` and `SystemProcesses::for_host` are gone (use
  `SystemProcesses::default()`).

- **Breaking:** **`isothrum-machine`'s `providers::Provider` is Isoloom's `isoloom_manifest::Provider`**,
  re-exported, in place of a copy of it. It is exhaustive (a `match` needs no `_` arm), and
  `Provider::id` is gone: a provider's id is `isothrum_runner::wire_name(&provider)`, the one
  helper that names a manifest value as the manifest writes it.

- **Breaking:** **The process port has one path, the crate root**: `Exit`, `ProcessError` and `OnLine` join
  `ProcessRunner`, `ProcessSpec` and `SystemProcesses` there, and the `process` module is
  internal. The `fake` feature's test doubles are `isothrum_runner::fake::{FakeProcesses, Reply,
  Call}` (were `process::fake::…`), and `Call` is `ProcessSpec` (a type alias): a call is
  recorded as the spec it ran, its fields and `line()` and `env(name)` as before.
- **Breaking:** **`AccessPick` and `AttachReport` are `#[non_exhaustive]`**: build an `AccessPick` from
  `AccessPick::default()` with its `with_*` methods.
- **`isothrum-machine`'s reasons speak to no particular program**: no "then re-check" or "run
  this program as administrator" ("run elevated").
- **Breaking:** **`ProcessError::Refused` carries `why`** in place of `arg`: besides an argument a wrapper
  would mangle, it now refuses an elevated call's standard input.
- **Breaking:** **`Error::PortInUse`'s `protocol` is a `String`** (was `&'static str`), the protocol as the
  manifest writes it (`tcp`, `udp`).

- **Breaking:** **`plan_json`, `own_files_json` and `choices_json` throw if their JSON can't be written.** Each
  had its own stand-in (`[]`, `{}`, a failed plan, else an empty string); they now return
  `Result<String, JsError>`. JavaScript gets the same string as before, and in the case that can't
  happen, an `Error` saying why.
- **Breaking: the compiler's `Error::ReservedLanName` is `ReservedName`**: a switch's name may not
  start with `link-` either. Its message says the prefix is the cables'.
- **Breaking:** **`own_files_json` throws for a topology that is malformed or doesn't compile.**
  It returned `[]`, so a caller couldn't tell a project with no file of its own from a drawing
  that failed; it now throws an `Error` saying why, as `bundle` does.
- **`cargo run -p xtask -- wasm --copy-to <dir>`** replaces `--website <dir>`: it copies the
  files a page serves (`isothrum_wasm.js`, `isothrum_wasm_bg.wasm`) into the folder given, not
  into a site's `public/wasm/`.

### Removed

- **Breaking:** The run-manifest values Isoloom never writes are no longer read: a `static` output, an expiry in
  minutes from now, a dotenv state file and a WinRM guest delivery (the next Isoloom release
  removes them from the manifest).
- **Breaking:** **`Running::shell_command`.** The access machine's shell is `Running::console_command` of its
  name (the `AttachReport`'s `machine`), which starts bash where the machine has it, else sh.
  `isothrum shell` goes with it: use `isothrum console <machine>`.

- **Breaking:** **`isothrum-machine` keeps only provider detection.** Its public API is now
  `providers::{detect, Provider, ProviderStatus}` (the crate root no longer re-exports
  `Provider` and `ProviderStatus`: import them from `providers`). The download record's format
  goes too (`storage::{Owned, OwnedBox, OwnedImage, load, parse_box_list}`): half a format, its
  writer in the one program that keeps the record, which now holds all of it. Gone, with no user
  left: the self-tests (`selftest`, `BootProbe`, `ElevatedBoot`, `Vm`), the system report
  (`system`, `SystemReport`, `DockerDenied`), `Dirs`, `Error` and `Result`, the `exec` module,
  the record's writing half (`storage::{path, update, Before, image_ids, box_list, claim,
  tracked}`), and the provider helpers `parse_plugins`, `parse_plugin_versions`,
  `version_at_least`, `kvm_problem` and `HYPERV_NEEDS_ADMIN` (still used inside `detect`).
- **Breaking:** **The `Hooks` port** (`Hooks`, `HookError`, `RunView`, `Runner::add_hooks`) and the
  `hook_failed` warning code (gone from the event schema): nothing implemented it. An embedder
  that keeps something beside a run does it around the operation's call.
- **Breaking:** **`RunRecord::manifest_sha256` and `LoadedManifest::sha256`**: nothing read the hash. A record
  written with it still loads.
- **Breaking:** **`Serialize` and `Deserialize` on `isothrum-machine`'s `ProviderStatus`**: nothing
  wrote or read it as JSON (an embedder reads its fields). `Provider`, Isoloom's own, keeps both.
- **Breaking:** **`isothrum-machine`'s `Provider::Proxmox`.** There is no Vagrant Proxmox provider in use:
  detection always reported it unavailable (`vagrant-proxmox` not installed). A Proxmox run is
  Terraform's.
- **Breaking:** **`isothrum-machine`'s cloud providers.** `Provider::{Aws, Azure, Gcp, DigitalOcean, Linode,
  Oci}` and `Provider::is_cloud` go: detection never reports them (a cloud run is Terraform's, not
  a Vagrant provider's).
- **Breaking:** **`isothrum-machine`'s `ProviderStatus::available`**: it was `reason.is_none()`; read that.
- **Breaking:** **`Compiled::yaml`.** `Compiled::into_parts` gives the spec's text, with the spec and the
  project's files.
- **Breaking:** **`Runner::in_flight`, `process::capture` and `process::Captured`** are internal now: no
  embedder read them.
- **Breaking:** **`isothrum-wasm`'s Rust functions are internal**: `plan`, `bundle_bytes`,
  `bundle_target_bytes`, `own_files` and `choices`, with `Error` and `Result`. Only the
  `#[wasm_bindgen]` exports called them; those stay, as do `Plan`, `File` and `Choices`, the
  shape of the JSON the exports hand out.
- **Breaking:** **`LoopbackPorts` and `HeldPort` are internal, and `HostPorts` is sealed, its
  questions (`in_use`, `pick_free`, `reachable`) the runner's own.** No embedder implemented
  `HostPorts`, asked it anything or named the loopback's: `RunnerConfig::new` installs it, and a
  test sets `RunnerConfig::ports` to the `fake` feature's `FakePorts`.
- **Breaking:** **`ShellCommand` is `#[non_exhaustive]`**: build one with `ShellCommand::new(program,
  args, cwd)` and `with_env`; its fields stay readable.
- **Breaking:** **`RunnerConfig::with_processes` and `RunnerConfig::with_ports`.** Set `RunnerConfig::processes`
  and `RunnerConfig::ports` on `RunnerConfig::new(home)`, as every other field is set.
- **Breaking:** **Runner API nothing used:** `Running::outputs` (read `Running::status`'s `outputs`),
  `RunnerConfig::with_clock`, `JsonLines` (an `Events` sink is a closure), `FixedClock` (now the
  crate's own test double), and `this_host` (now internal).
- **Breaking:** **More API nothing used.** In `isothrum-runner`: the `Clock` port (`Clock`, `SystemClock`,
  `RunnerConfig::clock`: the runner reads the system's clock), `Recorder`, `EVENTS_VERSION` (an
  event's `v`), `Running::path`, and the test doubles' `FakePorts::take`, `FakePorts::answer`,
  `Reply::after` and `FakeProcesses::lines` (read `FakeProcesses::calls`), and `From<Option<i32>>`
  for `process::Exit` (use `Exit::new`). In
  `isothrum-compiler`: `Compiled::spec` (`Compiled::into_parts` gives the spec). In
  `isothrum-machine`: `Provider::ALL`, `Provider::is_remote`, and `ProviderStatus`'s `remote`,
  `hypervisor`, `plugin`, `plugin_installed`, `hypervisor_version` and `plugin_version` (its
  `reason` says what is missing or too old).
- **Breaking:** **The ad-hoc access machine.** The runner no longer stands up an access machine
  of its own when a run declares none: no `<run>-isoloom-access` container on the run's network
  (on this computer's Docker, inside a `docker-vm` VM, inside a VM Terraform made on a server or
  a cloud), no `isoloom-access` VM added to a VM run through Isoloom's Vagrantfile hook (gone from
  Isoloom's Vagrantfiles too), and status no longer probes for one; `down` and `park` no longer
  reap one. `attach` and `detach` on a run that declares no access machine now fail with
  `unsupported` ("this run declares no access machine"). A run's own on-demand access machine
  (`access: true`: a Compose service in the `access` profile, a Vagrant define) is unchanged. An
  embedder adds a machine of its own as an ordinary machine of the run, with `Running::update`,
  `Running::retire` and `Running::console_command`.
- **The desktop app's crate, `apps/desktop`.** The Isothrum desktop app lives in its own
  repository, `isoloom/isothrum-desktop`, which ships it (installers, the Microsoft Store) on the
  crates published from here; the copy kept here "while that move completes" had drifted from it
  and had no release path. Its CI jobs, the installer workflow and its place in the xtask
  dependency rule go with it. The 2026-11-08 fixes made on that copy (the editor's command names,
  the hypervisor choice, UTM's state) are in the desktop repository.

### Security

- **tokio 1.54.1** in `Cargo.lock`, past RUSTSEC-2026-0386 (`AsyncFd`'s constructors; Isothrum
  doesn't call them). `cargo deny` allows crates.io only, Isoloom's git repository no longer.

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
