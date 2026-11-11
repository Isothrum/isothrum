# Architecture

Isothrum runs environments that [Isoloom](https://github.com/isoloom/isoloom) describes. The two
split the work:

- **Isoloom** reads one spec and writes the native files of each target (a Compose file, a
  Vagrantfile, a Terraform module, cloud-init, checks) and a **run manifest**,
  `.isoloom/<target>/run.json` (`.isoloom/<target>/<cloud>/run.json` for a cloud target, one per
  cloud).
- **Isothrum** reads the run manifest and nothing else of the generated files. It runs the steps
  the manifest lists, in order, with the tools the manifest names, and keeps the run's state.

The run manifest is the contract between them. Its format is defined by Isoloom's
`isoloom-manifest` crate and carries a `manifest_version`; Isothrum reads the one version the
`isoloom-manifest` it is built with accepts (see [Policies](../policies.md#run-manifest-versions)).

## What the manifest holds, and what the runner adds

The manifest holds the native commands in order (`up`, `down`, `park`, `resume`, `status`,
checks), the documented driver actions, readiness conditions, outputs (addresses, host ports,
URLs), the check runners, and the *slots* that params, inputs and secrets travel through. It never
holds a secret's value.

The runner adds what only it can know: the run's name and private state, one operation per run at
a time, the values of params and secrets, and the event stream.

## Crates

| Crate | Role |
|---|---|
| `isothrum-runner` | The engine: lifecycle, state and locks, drivers, secrets, checks, events. A library with no UI. |
| `isothrum-cli` | The `isothrum` binary: parses the command line, builds a runner, prints events, maps the outcome to an exit code. |
| `isothrum-machine` | Detects the Vagrant providers (hypervisors) a computer can drive. |
| `isothrum-compiler` | Turns a drawn topology into an Isoloom spec. Not involved in running. |
| `isothrum-wasm` | The compiler and Isoloom's targets in WebAssembly, for a browser or a JavaScript runtime. |

The dependency direction is one way: the CLI and `isothrum-machine` depend on the runner, and the
WebAssembly build on the compiler. Each crate's Isoloom dependencies, as its `Cargo.toml` has them:

| Crate | Isoloom crates |
|---|---|
| `isothrum-runner` | `isoloom-manifest` (`isoloom-targets` for its tests only) |
| `isothrum-cli`, `isothrum-machine` | `isoloom-manifest` |
| `isothrum-compiler` | `isoloom-spec`, `isoloom-manifest` (`isoloom-targets` for its tests only) |
| `isothrum-wasm` | `isoloom-spec`, `isoloom-manifest`, `isoloom-plan`, `isoloom-targets` |

## Inside the runner

- **Operations.** `Runner::open` reads and checks a manifest and returns a `Running`, whose
  methods are the operations (`up`, `down`, `park`, `resume`, `update`, `retire`, `check`,
  `provision`, `attach`, `detach`) and queries (`status`, `console_command`).
- **Executor.** Runs a list of steps: each step is an argv or a driver action, with its filters
  (host OS, provider, `when`), its timeout, its environment and its deliveries. A step's timeout is
  the time it may go without new output.
- **Drivers.** One per manifest driver: `compose`, `vagrant`, `terraform`. A driver implements its
  own actions and how it reads status; the steps themselves are generic. A driver may fetch what a
  step needs before it, in a step of its own with no ceiling while the download moves: Compose
  pulls the images of an `up` (`pull-<step>`), Vagrant downloads the boxes a `vagrant up` boots
  (`boxes-<step>`, those the manifest lists that aren't there yet), so a slow download isn't
  counted against the `up`. A step the manifest marks `download` (Compose's pull in a VM) runs
  under the same rule. A download that failed for the network (a connection reset, a gateway's
  502) runs again, up to three more times with a growing wait, in the executor for every driver;
  a refusal (a sign-in, a rate limit, a name the registry doesn't have) and a stall don't.
- **Ports.** Every outside effect goes through a trait: `ProcessRunner` (starting tools),
  `HostPorts` (the host's TCP and UDP ports) and `Events` (where events go). Tests use scripted fakes
  (the `fake` feature) and never start a real tool.
- **Store.** A plain folder per run under the runner's home: the record, a lock, the event log and
  the run's state folder. See [State layout](../reference/state.md).
- **Events.** Every operation emits JSON events and ends with exactly one `result` or `error`.
  See [Event stream](../reference/events.md).

## Processes

Each tool runs as a child process in its own process group (a job object on Windows), so stopping
an operation stops the whole tree it started. Its output is read line by line into `log` events.
The runner never starts a shell to run a step: argv goes to the OS as a list. On Windows the tools
that ship as `.cmd` wrappers (`az`, `gcloud`, `oci`) go through `cmd /C`, and an argument holding
one of cmd.exe's operators is refused.

## Concurrency

The runner is async, on Tokio. Operations on one run in one runner wait for each other. Across
processes, an OS file lock on the run keeps a second operation out: it waits a second for the
lock, then fails with `locked` (it never queues behind the first). Operations on different runs
proceed side by side.

## Where it runs the tools

The runner runs the host's tools (`docker`, `vagrant`, `terraform`, `ssh`) as the user who runs
it. When a manifest says a tool runs from a pinned container image on this host (Terraform for a
Proxmox target on macOS, where macOS's Local Network Privacy keeps third-party binaries off the
local network), the runner runs it with `docker run` instead, mounting only the run's folder, its
state folder, the runner's SSH folder and the Terraform plugin cache, plus, read-only, the CA
bundles and Terraform CLI configuration the host's environment names.
