# Event stream

Every operation reports what it does as JSON events. `isothrum <command> --json` prints them on
standard output, one object per line; an embedder of `isothrum-runner` receives them through its
`Events` sink. Each run also keeps the last operations' events in `<home>/runs/<run>/events.jsonl`
(`isothrum events` prints them, and says on standard error how many lines it couldn't read: one
another version of Isothrum wrote, or one cut short).

The authoritative definition is the JSON Schema committed at
[`crates/isothrum-runner/schema/events-v0.schema.json`](../../crates/isothrum-runner/schema/events-v0.schema.json)
(also `isothrum_runner::events_schema()`). A test fails when the code and the file differ;
`cargo run -p xtask -- schema` regenerates it.

## Common fields

| Field | Meaning |
|---|---|
| `v` | The stream's version: `0`. |
| `seq` | The event's place in the operation, from 1. |
| `time` | When, in UTC (`YYYY-MM-DDTHH:MM:SSZ`). |
| `run` | The run's name. |
| `op` | The operation: `up`, `down`, `park`, `resume`, `check`, `provision`, `attach`, `detach`, `update`, `retire`. |
| `event` | The kind, below. |

Every event an operation emits has `run` and `op`. They are absent only from an event written
outside an operation: with `--json`, a failure before any operation began (code `usage`, for
instance) is an `error` event too, and `isothrum reap --json` ends with a `result` whose `data`
is the sweep (`cloud`, `destroyed`, `pending`, `failed`, `orphans`, `unread`), after the events of
each run it destroyed; when the cloud couldn't be searched (a recorded run's state unreadable),
`orphans` and `unread` are null and an `error` without `run` nor `op` follows it. So one stream
carries every outcome, one event per line. `isothrum attach
--json` says where the machine is in its `result`'s `data` (`machine`, `native`, `network`,
`address`, `image`).

## Kinds

| `event` | Fields | Meaning |
|---|---|---|
| `state` | `from`, `to` | The run moved from one state to another (`starting`, `ready`, `failed`, `parking`, `parked`, `resuming`, `stopping`, `absent`). |
| `step` | `step`, `state`, `note` | A step started (`running`), finished (`done`), `failed` or was `skipped`. |
| `log` | `step`, `stream`, `line` | A line a step's tool printed on `stdout` or `stderr`. |
| `ready` | `condition`, `state`, `detail` | A ready condition was checked: `waiting` (not yet, checked again), `ok` (it holds) or `failed` (it doesn't, and the operation fails with `not_ready`). |
| `output` | `name`, `role`, `value`, `sensitive` | An output's value, once the run is up (`***` when sensitive). |
| `check` | `runner`, `from`, `name`, `ok`, `detail` | One check's result. |
| `warning` | `code`, `message` | Something worth showing that didn't fail the operation. |
| `result` | `ok`, `data` | The operation ran to its end. |
| `error` | `code`, `message`, `step`, `timeout` | The operation failed. `timeout` is set for a step stopped for no progress (`no_progress`, with its last line) or at its ceiling (`ceiling`). |

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
state (`starting` through an `up`), or the record's state before the operation records one and
for an operation that leaves the run as it was (`ready` while an `update` runs), and `machines`
are as the runner last read them (`seen.json` in the run's folder,
[State layout](state.md)). With no operation in flight, `live` is `true` and everything is read
now.

## Reading the stream

Read one line at a time and dispatch on `event`. Ignore event kinds and fields you don't know,
and treat a `code` you don't know as a generic warning or failure: later releases with the same
`v` may add kinds, fields and codes, but don't remove or rename them.

```sh
isothrum up --json | jq -c 'select(.event == "error" or .event == "result")'
```
