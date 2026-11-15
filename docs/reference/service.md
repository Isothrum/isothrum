# Runner service

`isothrum serve --home <home>` runs the home's **runner service**: one daemon per home that runs
every operation of the home (one at a time per run, runs side by side) and streams their events
to its clients. The command line starts it when it isn't running (unless `--no-service`); an app
links [`isothrum-client`](https://docs.rs/isothrum-client).

## Files

| In the home | What |
|---|---|
| `service.lock` | The OS lock the service holds for its life (released by the OS however it ends): a second service of the home exits naming the holder. |
| `service.json` | Written once it listens, removed when it stops: `pid`, `started` (RFC 3339, UTC), `version` (its release), `protocol`, `endpoint`, `epoch`. A dead service's may stay: a client checks it. |
| `service.sock` | Unix: the owner-only socket it listens on. Windows: a named pipe of a random name only the user may open, named in `service.json`. |
| `service.log` | What a service a client started writes. |
| `service.replace.lock` | Held by a client while it replaces a service of another release. |

## Protocol

Newline-delimited JSON (one object per line, at most 16 MiB). A client writes requests
`{"id":N,"cmd":"…",…}`; the service answers each with one reply carrying its id. The first request
is `hello`. The commands, replies and the event frames are documented in the client crate's
[`proto`](https://docs.rs/isothrum-client/latest/isothrum_client/proto/) module.

### A Terraform folder's input variables

`{"id":N,"cmd":"variables","target":{"path":"/abs/folder"}}` is answered
`{"reply":"variables","id":N,"variables":[…]}`: each `variable` block of the folder's `*.tf` and
`*.tf.json` files, by name, with its `type` as written, its `default` (JSON; absent when the module
needs it given), `sensitive` and `description`. Nothing runs and nothing is recorded. A folder that
holds no Terraform module is refused (`project_invalid`). A client gives a sensitive variable's
value as a secret (`options.secrets`), which no record keeps, and the others as values
(`options.env`), which the run's record keeps and every later operation gives again. Each reaches
the module as `TF_VAR_<name>`. In Rust: `Client::variables(target)`, `Project::variables()`.

### A sensitive output's value

A status read gives every output a module marks sensitive as `***`, as every event does. A client
that needs the value itself (a lab's admin password, to show its user) asks for it:
`{"id":N,"cmd":"status","run":"…","options":{"reveal":true}}`. That read's outputs then hold their
own values, still with `sensitive: true`. Nothing else changes: no event ever holds the value, no
record keeps it, and without `reveal` the read masks as before. The service gives it only to its
own account: its endpoint is owner-only (a Unix socket in the home's owner-only folder, a named
pipe with an owner-only ACL), so any client that can ask is the run's owner. In Rust:
`Client::status_with(run, target, OpOptions { reveal: true, .. })`, `RunOptions::with_reveal()`.

### The reaper

A run started with a lifetime (`--lifetime`, `RunOptions::lifetime`, an `up`'s `lifetime_s`)
records when it expires. The service looks for runs past it every 60 seconds by default
(`ServeOptions::reap_every`, `Some(Duration::from_secs(60))` from `ServeOptions::new`) and queues
each one's `down`, as a client would, unless the run has an operation queued or running. A `down`
it queued is not queued again for 15 minutes, so one that keeps failing doesn't run back to back.

The reaper's `down` gives the tools the values the run's record keeps (a Terraform module's
`region`, replayed as at every operation), and never a secret: no record keeps one. A run whose
`down` needs a secret (a cloud account's credentials) fails it, and is tried again 15 minutes
later; `isothrum reap`, given the secrets, removes it, as any client's `down` that passes them.

`reap_every: None` turns the reaper off: the service never looks for expired runs. An embedder that
reaps itself (with the account's credentials it holds) hosts the service so.

### What every protocol keeps

So that a client of one release can always tell which service runs and replace it, every protocol
keeps these two frames as they are:

- `{"id":N,"cmd":"hello","protocol":P}` is answered `{"reply":"hello","id":N,"protocol":Q,"version":"…"}`
  whatever `P` is. When `P` isn't the service's `Q`, the connection then takes only
  `shutdown_if_idle`; any other request is refused (`usage`).
- `{"id":N,"cmd":"shutdown_if_idle"}` is answered `{"reply":"ok","id":N}` when no operation is
  queued or running (the service takes no new one from then on, and exits), else
  `{"reply":"busy","id":N,"operations":[…]}` (their ids, oldest first) and nothing changes.

### Errors and their detail

A refused request is answered `{"reply":"error","id":N,"code":"…","message":"…"}`, and an operation
that fails ends with an `error` event (`{"kind":"error","code":"…","message":"…",…}`). Both keep
their `code` and `message` as before. When the runner's failure has typed fields, both also carry
a `detail` object:

```json
{"reply":"error","id":7,"code":"port_in_use","message":"…",
 "detail":{"v":1,"kind":"port_in_use","port":8080,"protocol":"tcp"}}
```

- `v` is the version of the fields (1). A later runner adding a field makes a new version and never
  changes what a field means.
- `kind` names the failure. Its fields are its own:
  - `port_in_use`: `port`, `protocol`.
  - `tool_missing`: `tool`.
  - `tool_too_old`: `tool`, `found`, `required`.
  - `tool_unanswered`: `tool`.
  - `timeout`: `timed_out`, without the tool's last line.
  - `download_stalled`: `after_s`.
  - `running_elsewhere`: `recorded`, `asked`.
  - `not_parkable`: `mode`.
  - `partly_gone`: `machines`.
  - `secret_missing`: `name`.
  - `input_invalid`: `name`.
  - `project_invalid`: `dir`.
  - `machine_unknown`: `machine`.
  - `network_unknown`: `network`.
  - `provider_refused`: `provider`.
  - `no_console`: `machine`.
  - `server_auth_failed`: `server`.
  - `io`: `path`, `action`.
  - `state_conflict`: `expected`, `found`.
  - `lease_held`: `scope`, `pid`.
- `detail` never holds a secret, nor any text a tool printed (which might echo one): those stay in
  `message`, which the event stream masks. It is absent when the failure has no typed fields.

The Rust types are `isothrum_runner::ErrorDetail` and `isothrum_runner::Detail`. The client hands
them on as `ClientError::Refused { detail, .. }` and `ClientError::Operation { detail, .. }`.

### Stopping

| Request | Operations queued or running |
|---|---|
| `shutdown` | Queued ones are dropped, running ones cancelled and their tools stopped (40 seconds, then ended at once), then the service exits. It never refuses. |
| `shutdown_if_idle` | Refused with `busy`: the service goes on. |

The service also stops on Ctrl-C or SIGTERM (as `shutdown`), and after `--idle-timeout` seconds
with no client and no operation.

## Replacing a service of another release

`Client::connect` takes the service it finds, whatever its release; it fails only on another
protocol. `Client::connect_exact(home, binary)` connects only to a service of the client's release
(`isothrum_client::VERSION`) and protocol, for an app that bundles its own `isothrum`:

1. It takes the home's replacement turn (`service.replace.lock`, up to a minute): two apps replace
   a service one after the other.
2. It connects to the service `service.json` names and says `hello` (3 seconds to answer).
   - The client's release and protocol: that connection is the client.
   - Another release or protocol: it asks `shutdown_if_idle`. `busy` ends the call with
     `ClientError::Busy { version, pid, operations }`, the service left running, for the app to
     tell its user and try again later. `ok`: it waits (20 seconds) until the service is gone, its
     lock released and its endpoint closed.
   - No answer while a process holds `service.lock`: it waits 5 seconds (a service starting up
     holds the lock before it listens), then takes it as hung. It reads the journal (`state.db`,
     read-only, never migrated): an operation of that pid unfinished, or a journal it can't read,
     and the service is left running (`ClientError::Busy`, or `ClientError::Unavailable`). Else it
     ends the pid (SIGTERM, then SIGKILL after 5 seconds; Windows: `taskkill /F`), but only while
     the lock is still held and the process with that pid started no later than `started` (within
     2 seconds): a pid is reused only after its process ended, so one reused since started later
     and is never ended (`ClientError::Unavailable`, nothing ended).
   - No process holds `service.lock`: there is no service, whatever `service.json` says (a stale
     one is never acted on).
3. With no service, it starts `binary` (`serve --home <home> --detach`, as `Client::connect` does),
   connects, and checks the service's `hello`: another release is `ClientError::Version`, and that
   service is asked to stop again.

The app reads none of the home's files itself.
