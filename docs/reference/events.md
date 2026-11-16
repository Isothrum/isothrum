# Event stream

Every operation reports what it does as JSON events. `isothrum <command> --json` prints them on
standard output, one object per line; an embedder of `isothrum-runner` receives them through its
`Events` sink. The home's store (`state.db`, [State layout](state.md)) also keeps each run's
events, its newest 2000 once over 4000 (`isothrum events` prints them, and says on standard error
how many it couldn't read: an event kind or a code another version of Isothrum wrote).

The authoritative definition is the JSON Schema committed at
[`crates/isothrum-runner/schema/events-v1.schema.json`](../../crates/isothrum-runner/schema/events-v1.schema.json)
(also `isothrum_runner::events_schema()`). A test fails when the code and the file differ;
`cargo run -p xtask -- schema` regenerates it.

## Common fields

| Field | Meaning |
|---|---|
| `v` | The stream's version: `1` (`0` before 3.0; see [Policies](../policies.md#what-is-stable)). |
| `seq` | The event's place in the operation, from 1. |
| `time` | When, in UTC (`YYYY-MM-DDTHH:MM:SSZ`). |
| `run` | The run's name. |
| `op` | The operation: `up`, `down`, `park`, `resume`, `check`, `provision`, `attach`, `detach`, `update`, `retire`. |
| `operation` | The operation's id in the home's journal, on every event of a journaled operation (one the runner service queued). |
| `event` | The kind, below. |

Every event of an operation has `run` and `op`. An event written outside one has those its
writer knows. The command line's own: a command line it refused has neither; `reap`'s closing
`result` and `error` have neither; any other failure before an operation began has `op` when the
command is an operation, and `run` when `--name` named it (a failure of `status`, `console`,
`events` or `key` has no `op`).

With `--json`, a failure before any operation began (code `usage`, for instance) is an `error`
event too, and `isothrum reap --json` ends with a `result` whose `data` is the sweep
(`destroyed`, `pending`: what `--dry-run` would destroy, `failed`: each `run` and its `error`),
after the events of each run it destroyed. So one stream carries every outcome, one event per
line.

A `result`'s `data` has one shape per operation, told apart by its fields: `up`'s is `state`
(`ready`) and `outputs` (each `name` and `value`, `***` when sensitive, null when it can't be
read); `exec`'s is `machine`, the machine it ran in; `reap`'s is the sweep above; every other
operation's is `state`, where the run stands now.

## Kinds

| `event` | Fields | Meaning |
|---|---|---|
| `state` | `from`, `to` | The run moved from one state to another (`creating`, `ready`, `failed`, `parking`, `parked`, `resuming`, `updating`, `destroying`, `absent`). |
| `step` | `step`, `state`, `note` | A step started (`running`), finished (`done`), `failed` or was `skipped`. |
| `log` | `step`, `stream`, `line` | A line a step's tool printed on `stdout` or `stderr`. |
| `desired` | `desired` | What the user wants of the run changed, as an operation that moves it began: `running` (`up`, `resume`), `parked` (`park`), `gone` (`down`). |
| `resource` | `key`, `kind`, `state`, `native_id` | A resource of the run moved in the home's write-ahead ledger ([State layout](state.md)): `creating` before the call that makes it, `created` with the tool's id (`native_id`) after, `destroying` before the call that removes it, `gone` after. `kind`: `vm`, `container`, `network`, `volume`, `hub`, `process`, `module`, `host_keys`, `state_files`, `archive`, `consoles`, `tool_process`. |
| `warning` | `code`, `message` | Something worth showing that didn't fail the operation. |
| `result` | `ok`, `data` | The operation ran to its end. `ok` is false when it didn't reach its goal (a check failed, an `exec`'s command exited non-zero); `data` is above. |
| `error` | `code`, `message`, `step`, `detail`, `changed` | The operation failed ([the report](service.md#errors-and-their-detail)). A step stopped for no progress or at its ceiling has the `detail` `timeout`: its `timed_out` (`kind` `no_progress` or `ceiling`), with the limit in seconds (`after_s`) and the step's last line (`last_line`, when it printed one). `changed` is false for a refusal made before anything acted on the run. A download made before a step in a step of its own (`pull-<step>`: a Compose `up`'s images; `boxes-<step>`: the boxes a `vagrant up` boots), or a step the manifest marks `download`, has no ceiling: stopped for no progress, its code is `download_stalled`, the message quoting its last line; failed, `download_failed`. `detail` is the failure's typed fields, when it has any (`v`, `kind` and its own fields; see [Runner service](service.md#errors-and-their-detail)). |
| `operation` | `phase`, `outcome`, `position` | The runner service's, not an operation's: an operation it journals was `queued` (`position` in its run's queue, 0 next), `started` or `ended` (`outcome`: `succeeded`, `failed`, `cancelled`, `abandoned`). The operation's own events still end with exactly one `result` or `error`. |
| `output` | `name`, `value`, `sensitive` | An output's value (`***` when sensitive). The runner no longer writes it: `up`'s `result` carries the outputs. |

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

`isothrum status --json` prints one JSON document, not events. Its schema is
[`crates/isothrum-runner/schema/status.schema.json`](../../crates/isothrum-runner/schema/status.schema.json)
(`isothrum_runner::status_schema()`).

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
