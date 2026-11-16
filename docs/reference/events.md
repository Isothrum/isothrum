# The `--json` output

`isothrum <command> --json` prints JSON on standard output, one document per line, and nothing
else there. It is a stable interface ([Policies](../policies.md#what-is-stable)): what each command
prints, its fields and its codes are defined by the JSON Schema
[`json-output.schema.json`](json-output.schema.json), written from the types that print them. A
test fails when the code and the committed schema differ;
`ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it again, with the tables
below.

| Command | Prints | Schema |
|---|---|---|
| An operation: `up`, `down`, `park`, `resume`, `update`, `retire`, `exec`, `provision`, `attach`, `reap` | Its events, one per line, ending with exactly one `result` or `error`. | `Event` |
| `events` | The run's recorded events, oldest first. | `Event` |
| `status` | One document. | `Status` |
| `console` | One document: the command that opens the console. | `ShellCommand` |
| `screen` | One document: where the screen answers. | `Screen` |
| `screen --app` | One document: what opens the hypervisor's app. | `NativeApps` |
| `key` | One document: the public key. | `PublicKey` |
| Any command that fails before its operation began (code `usage`, for instance) | One `error` event. | `Event` |

An embedder of `isothrum-runner` receives the same events through its `Events` sink
(`isothrum_runner::events_schema()`, also committed as
[`crates/isothrum-runner/schema/events-v1.schema.json`](../../crates/isothrum-runner/schema/events-v1.schema.json)).
The home's store (`state.db`, [State layout](state.md)) keeps each run's events, its newest 2000
once over 4000 (`isothrum events` prints them, and says on standard error how many it couldn't
read: an event kind or a code another version of Isothrum wrote).

## Events

Every event of an operation has `run` and `op`. An event written outside one has those its
writer knows. The command line's own: a command line it refused has neither; `reap`'s closing
`result` and `error` have neither; any other failure before an operation began has `op` when the
command is an operation, and `run` when `--name` named it (a failure of `status`, `console`,
`events` or `key` has no `op`).

`isothrum reap --json` ends with a `result` whose `data` is the sweep (`destroyed`, `pending`:
what `--dry-run` would destroy, `failed`: each `run` and its `error`), after the events of each run
it destroyed. So one stream carries every outcome, one event per line.

A `result`'s `data` has one shape per operation, told apart by its fields: `up`'s is `state`
(`ready`) and `outputs` (each `name` and `value`, `***` when sensitive, null when it can't be
read); `exec`'s is `machine`, the machine it ran in; `reap`'s is the sweep above; every other
operation's is `state`, where the run stands now. `ok` is false when the operation ran to its end
without reaching its goal (an `exec`'s command exited non-zero).

An `error` is the failure's report ([Runner service](service.md#errors-and-their-detail)): its
`code` (every code, its cause and its remedy: [Exit codes](exit-codes.md)), its `message`, the
`step` it happened in, its typed `detail` and whether it `changed` the run (false for a refusal
made before anything acted on it). A step stopped for no progress or at its ceiling has the detail
`timeout`. A download made in a step of its own (`pull-<step>`: a Compose `up`'s images;
`boxes-<step>`: the boxes a `vagrant up` boots) has no ceiling: stopped for no progress, its code
is `download_stalled`; failed, `download_failed`.

The tables below are written from the schema. A field marked `?` may be absent or null.

<!-- Written from the schema: don't edit down to the end marker. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->

### Common fields

| Field | Meaning |
|---|---|
| `op` | The operation: on every event of an operation; outside one, when its writer knows it (see `Event`). Optional. |
| `operation` | The operation's id in the home's journal: on every event of a journaled operation. Optional. |
| `run` | The run's name: on every event of an operation; outside one, when its writer knows it (see `Event`). Optional. |
| `seq` | Its place in the operation, from 1. |
| `time` | When (`YYYY-MM-DDTHH:MM:SSZ`, UTC). |
| `v` | The stream's version (1). |

### Kinds

| `event` | Fields | Meaning |
|---|---|---|
| `state` | `from`, `to` | The run moved from one state to another. |
| `step` | `note`?, `state`, `step` | A step started, finished, failed or was skipped. |
| `log` | `line`, `step`, `stream` | A line a step's tool printed. |
| `output` | `name`, `sensitive`, `value`? | An output's value, once the run is up. |
| `warning` | `code`, `message` | Something the embedder may show, that didn't fail the operation. |
| `result` | `data`, `ok` | The operation's outcome, when it ran to its end. |
| `operation` | `outcome`?, `phase`, `position`? | A journaled operation was queued, started or ended: what a runner service says of the operations it queues (the runner's own operation stream doesn't carry it: it still ends with exactly one `result` or `error`). |
| `desired` | `desired` | What the user wants of the run changed: an operation that moves it began (`up`, `resume`: running; `park`: parked; `down`: gone). |
| `resource` | `key`, `kind`, `native_id`?, `state` | A resource of the run moved in the home's write-ahead ledger: written `creating` before the call that makes it, `created` with the tool's id after it, `destroying` before the call that removes it, `gone` after. |
| `error` | `changed`?, `code`, `detail`?, `message`, `step`? | Why the operation failed: its code, message, step, typed fields and whether the run was touched (`crate::ErrorReport`). |

### Error details

An `error`'s `detail`, when the failure has typed fields: `v`, its `kind`, and the kind's own fields.

| `kind` | Fields | Meaning |
|---|---|---|
| `port_in_use` | `holder`?, `port`, `protocol` | A host port the run publishes is taken. |
| `tool_missing` | `tool` | A tool isn't installed. |
| `tool_too_old` | `found`, `required`, `tool` | A tool is older than the run needs. |
| `tool_unanswered` | `tool` | A tool's service didn't answer (Docker's engine). |
| `timeout` | `timed_out` | A step ran out of time. |
| `download_stalled` | `after_s`, `what` | A download showed no progress for as long as a download may. |
| `download_failed` | `reason`, `what` | A download couldn't be made. |
| `elevation` | `reason` | The elevation the run's commands need couldn't be had. |
| `not_ready` | `condition`, `detail` | A ready condition didn't hold. |
| `running_elsewhere` | `asked`, `recorded` | The run is recorded on another place than the one asked for. |
| `not_parkable` | `mode` | The run can't be parked in that mode. |
| `partly_gone` | `machines` | Machines of the run are gone, so it can't be resumed whole. |
| `secret_missing` | `name` | A secret the run needs has no value. |
| `input_invalid` | `name`, `reason` | A value the run was given can't be used. |
| `tool_failed` | `reason` | A step's tool ran and failed. |
| `project_invalid` | `dir` | The folder isn't a project to run. |
| `machine_unknown` | `machine` | A machine the run doesn't have. |
| `network_unknown` | `network` | A network the run doesn't have. |
| `provider_refused` | `provider` | A Vagrant provider refused. |
| `no_console` | `machine` | A machine has no graphical console to reach. |
| `server_auth_failed` | `server`? | The server the run is on refused its credentials. |
| `io` | `action`, `path` | A file or folder couldn't be read or written. |
| `state_conflict` | `expected`, `found`? | The run moved between a read and the write that would have moved it. |
| `lease_held` | `pid`, `scope` | A live process holds the lease asked for. |

<!-- End of what is written from the schema. -->

## Rules

- An operation ends with exactly one `result` or `error`. One cut short (Ctrl-C, SIGTERM, an
  embedder dropping it) ends with an `error` whose code is `cancelled`.
- A secret's value never appears: it is masked as `***` before the event is built.
- `log` lines are decoded lossily (invalid UTF-8 is replaced).
- A `log` line ends at `\n`, `\r\n` or a lone `\r`. A line ended by a lone `\r` is one a tool
  redraws in place (a progress meter): it is a `log` event only when its text changed, and at
  most once a second for each stream.
- `code` values are stable strings (see [Policies](../policies.md#what-is-stable)). The command
  line's exit codes are in [Exit codes](exit-codes.md); the warning codes are
  the schema's `WarningCode`.

## Status

`isothrum status --json` prints one JSON document, not events: `Status` in the schema (also
`isothrum_runner::status_schema()`, committed as
[`crates/isothrum-runner/schema/status.schema.json`](../../crates/isothrum-runner/schema/status.schema.json)).

While an operation holds the run (in this process or another), `status` doesn't ask the run's
tools, which that operation is acting with: Vagrant refuses to read a VM's state for as long as a
`vagrant up` boots and provisions it. `live` is then `false`, `state` is the operation's moving
state (`creating` through an `up`, `updating` through an `update`), or the run's recorded state
before the operation records one and for an operation that leaves the run as it was (`ready`
while an `attach` runs), and `machines` are as the runner last read them (the run's last
observation in the home's store, `state.db`, [State layout](state.md)). With no operation in
flight, `live` is `true`, everything is read now, and the run's recorded state follows what was
read where the state machine allows (status = reconcile(desired, observed)): `desired` says what
the user last asked for (`running`, `parked`, `gone`) and `failed_cause` why a failed run failed.

## Reading the stream

Read one line at a time and dispatch on `event`. Ignore event kinds and fields you don't know,
and treat a `code` you don't know as a generic warning or failure: later releases with the same
`v` may add kinds, fields and codes, but don't remove or rename them.

```sh
isothrum up --json | jq -c 'select(.event == "error" or .event == "result")'
```
