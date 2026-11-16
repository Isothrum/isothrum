# Architecture

Isothrum runs Compose projects, Vagrantfiles and Terraform modules, whoever wrote them, through
each format's own tool (`docker compose`, `vagrant`, `terraform`). A folder's files say its format
(`Format::detect`); the runner reads them as that tool would and nothing else, runs the tool, and
keeps the run's state. What Isothrum is for, and what stays out of it:
[what Isothrum is, and what it is not](boundaries.md).

## What the project holds, and what the runner adds

The project holds what its tool reads: the services or machines, the module's variables and
outputs. It never holds a secret's value.

The runner adds what only it can know: the run's name, key and private state, one operation per
run at a time, the values of variables and secrets (delivered without a command line), and the
event stream.

## Crates

| Crate | Role |
|---|---|
| `isothrum-runner` | The engine: lifecycle, state and locks, the format drivers, secrets, checks, events. A library with no UI. |
| `isothrum-service` | The runner service: one daemon per runner home that queues each run's operations, runs them, and streams their events. |
| `isothrum-client` | The service's client: connect to (or start) a home's service, send it commands, follow its events. |
| `isothrum-cli` | The `isothrum` binary: parses the command line, drives the runner or the service, prints events, maps the outcome to an exit code. |
| `isothrum-machine` | This computer's hypervisors and tools: detects the Vagrant providers it can drive, and why one can't. |

The dependency direction is one way: every crate depends on the runner, and the runner on none
of them. No crate depends on an Isoloom crate (`cargo xtask check`'s `isoloom-free` step, and the
`boundary` guardrail).

## Inside the runner

- **Operations.** `Runner::open` reads and checks a project folder and returns a `Running`, whose
  methods are the operations (`up`, `down`, `park`, `resume`, `update`, `retire`, `check`,
  `provision`, `attach`, `detach`) and queries (`status`, `console_command`).
- **Executor.** Runs a list of steps: each step is an argv or a driver action, with its filters
  (host OS, provider, `when`), its timeout, its environment and its deliveries. A step's timeout is
  the time it may go without new output.
- **Drivers.** One per format: `compose`, `vagrant`, `terraform`. A driver implements its
  own actions and how it reads status; the steps themselves are generic. A driver may fetch what a
  step needs before it, in a step of its own with no ceiling while the download moves: Compose
  pulls the images of an `up` (`pull-<step>`), Vagrant downloads the boxes a `vagrant up` boots
  (`boxes-<step>`, those that aren't there yet), so a slow download isn't
  counted against the `up`. A download that failed for the network (a connection reset, a gateway's
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
it. When a tool runs from a pinned container image on this host (Terraform for a
Proxmox target on macOS, where macOS's Local Network Privacy keeps third-party binaries off the
local network), the runner runs it with `docker run` instead, mounting only the run's folder, its
state folder, the runner's SSH folder and the Terraform plugin cache, plus, read-only, the CA
bundles and Terraform CLI configuration the host's environment names.
