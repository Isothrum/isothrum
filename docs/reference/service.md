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
| `service.sock` | Unix: the owner-only socket it listens on. A home on a path too long for a socket (over 103 bytes on macOS, 107 on Linux) listens in `isothrum-<uid>/` of the user's runtime folder (`XDG_RUNTIME_DIR`) or temporary folder instead, owner-only, named in `service.json`. Windows: a named pipe of a random name only the user may open, named in `service.json`. |
| `service.log` | What a service a client started writes. |
| `service.replace.lock` | Held by a client while it replaces a service of another release. |

## Protocol

Newline-delimited JSON (one object per line, at most 16 MiB). A client writes requests
`{"id":N,"cmd":"…",…}`; the service answers each with one reply carrying its id. The first request
is `hello`. The commands, replies and the event frames are documented in the client crate's
[`proto`](https://docs.rs/isothrum-client/latest/isothrum_client/proto/) module.

### Everything else a client asks of a home

A client never opens the home's files: what it would read or write there, it asks the service.
None of these is queued; each is answered at once.

| Request | Reply | In Rust |
|---|---|---|
| `{"cmd":"record","run":"…"}` | `{"reply":"run","run":{…}}`, `null` for a run never recorded | `Client::record` |
| `{"cmd":"forget","run":"…"}` | `ok`; `locked` while an operation of the run is queued or running. What the run created is left as it is | `Client::forget` |
| `{"cmd":"expired"}` | `{"reply":"runs","runs":[…]}`: the runs past their lifetime that may still hold something | `Client::expired` |
| `{"cmd":"console_command","run":"…","machine":"…","user":"…"}` | `{"reply":"shell","command":{program,args,cwd,env}}`, for the client to run in its own terminal | `Client::console_command` |
| `{"cmd":"graphical_console","run":"…","machine":"…","options":{…}}` | `{"reply":"console","console":{machine,protocol,endpoint,user,password,note,tunnelled}}` | `Client::graphical_console` |
| `{"cmd":"native_console","run":"…","machine":"…","options":{…}}` | `{"reply":"native","console":{machine,app,ways}}` | `Client::native_console` |
| `{"cmd":"service_endpoints","run":"…","options":{…}}` | `{"reply":"endpoints","endpoints":[{name,address,forwarded,why}]}`: where this computer reaches each published service, a forward through the run's jump host kept open by the service | `Client::service_endpoints` |
| `{"cmd":"public_key"}` | `{"reply":"public_key","key":"ssh-ed25519 …"}`, the runner's key made when it has none | `Client::public_key` |
| `{"cmd":"downloads"}` | `{"reply":"downloads","downloads":[{id,kind,…,run,fetched_at,size_bytes}]}`: what the runs' own downloads fetched, as the store records them (`kind` `image`: `reference`, `image_id`, `docker_context`; `kind` `box`: `name`, `provider`, `version`, `architecture`), each with its size when it can be read | `Client::downloads` |
| `{"cmd":"remove_downloads","which":"all"}`, or `"which":{"only":[id,…]}` | `{"reply":"downloads_removed","removed":[{id,outcome,…,download}]}`: each one asked, `outcome` `removed`, `gone` (removed outside, record forgotten), `moved` (`now`: the image reference stands for another id; nothing removed, record forgotten), `failed` (`reason`: in use, or the tool failed; kept with its record) or `not_recorded` | `Client::remove_downloads` |
| `{"cmd":"adopt_downloads","adopted":[{"kind":"image","reference",image_id?} or {"kind":"box","name","provider","version"},…]}` | `{"reply":"downloads_adopted","adopted":[{id,kind,…,fetched_at}]}`: downloads fetched outside the runs (an earlier program's own record) recorded with no `run`, each one on this computer now as it was fetched (an image while its reference stands for the `image_id` given, a box in each architecture Vagrant holds it); the downloads recorded for them | `Client::adopt_downloads` |
| `{"cmd":"address_blocks_in_use","except"?,"docker_context"?}` | `{"reply":"address_blocks","blocks":["172.17.0.0/16",…]}`: the IPv4 blocks the networks of the engine `docker_context` reaches hold (the current one when none), but those of run `except`'s own networks: what a deployment's networks must stay clear of | `Client::address_blocks_in_use` |

A removal takes only what the store records, each on its own: an image by its reference
(`docker image rm`, never forced, in the context it was pulled through) while the reference still
stands for the recorded id, a box by name, provider, version and architecture (`vagrant box
remove`, never `--force` nor `--all`). One a container or a machine uses is refused by its tool,
said in its outcome, and the others go on. In Rust, without the service: `Runner::downloads`,
`Runner::remove_downloads`, `Runner::adopt_downloads`.

`console_command`, `graphical_console`, `native_console` and `service_endpoints` take a `target`
too, for a run opened from its folder rather than its record. A graphical console reached through
an SSH tunnel keeps the tunnel open for as long as the connection that asked:
`Client::graphical_console` asks on a connection of its own, held by the `HeldConsole` it returns,
and dropping it ends the tunnel. When the tunnel ends first, the service writes
`{"reply":"console_closed","id":N,"reason":"…"}` and closes that connection
(`HeldConsole::closed`). The console's password travels like a revealed output: only over the
owner-only endpoint, and never in a debug form.

### A Terraform folder's input variables

`{"id":N,"cmd":"variables","target":{"path":"/abs/folder"}}` is answered
`{"reply":"variables","id":N,"variables":[…]}`: each `variable` block of the folder's `*.tf` and
`*.tf.json` files, by name, with its `type` as written, its `default` (JSON; absent when the module
needs it given), `sensitive` and `description`. Nothing runs and nothing is recorded. A folder that
holds no Terraform module is refused (`project_invalid`). A client gives a sensitive variable's
value as a secret (`options.secrets`), which no record keeps, and the others as values
(`options.env`), which the run's record keeps and every later operation gives again. Each reaches
the module as `TF_VAR_<name>`. In Rust: `Client::variables(target)`, `Project::variables()`.

### Status reads

`status` isn't queued, but the reads of one run take turns: never two of one run at once (Vagrant
refuses a read of a machine another read holds). A read asked while another of the same run runs
waits, and when it was asked with the same `target` and `options` as the next read to begin, it
takes that read's answer. A failed read stands for no other.

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
reaps itself (with the account's credentials it holds) hosts the service so, and sweeps through
the client: `expired`, then a `down` of each with the secrets it holds.

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

A refused request is answered `{"reply":"error","id":N,…}`, and an operation that fails ends with
an `error` event (`{"event":"error",…}`). Both carry the same report of the failure, the one the
runner's API gives (`Error::report`, `isothrum_runner::ErrorReport`):

- `code` and `message`;
- `step`, the step that failed, when one did;
- `detail`, the failure's typed fields, when it has any;
- `changed`: false when the failure is a refusal made before anything acted on the run (another
  operation holds it, it is already up or isn't up, it is recorded elsewhere, nothing is left to
  resume, a value it needs is missing or unusable, a tool it needs is missing or refuses this
  account, a cloud refuses the sign-in), so whatever the
  embedder prepared for the operation can be put back; true otherwise. A service refusal is
  always `false`.

```json
{"reply":"error","id":7,"code":"port_in_use","message":"…","changed":true,
 "detail":{"v":2,"kind":"port_in_use","port":8080,"protocol":"tcp",
           "holder":{"pid":4123,"program":"nginx"}}}
```

- `v` is the version of the fields (2). A later runner adding a field makes a new version and never
  changes what a field means.
- `kind` names the failure. Its fields are its own:
  - `port_in_use`: `port`, `protocol`, and `holder` (`pid`, `program`) when the process listening
    on it was found (`lsof` or `ss`, PowerShell on Windows, looked up once the operation failed).
  - `tool_missing`: `tool`.
  - `tool_too_old`: `tool`, `found`, `required`.
  - `tool_unanswered`: `tool`.
  - `timeout`: `timed_out` (`kind` `no_progress` or `ceiling`, `after_s`, and `last_line` when the
    step printed one).
  - `download_stalled`: `what` (`images` or `boxes`), `after_s`.
  - `download_failed`: `what`, `reason`.
  - `elevation`: `reason`.
  - `not_ready`: `condition`, `detail`.
  - `running_elsewhere`: `recorded`, `asked`.
  - `not_parkable`: `mode`.
  - `partly_gone`: `machines`.
  - `secret_missing`: `name`.
  - `input_invalid`: `name`, `reason`.
  - `tool_failed`: `reason`.
  - `project_invalid`: `dir`.
  - `machine_unknown`: `machine`.
  - `network_unknown`: `network`.
  - `provider_refused`: `provider`, and `need` (`virtiofsd`) when the hypervisor found nothing
    this computer must have for it.
  - `tool_denied`: `tool`.
  - `credentials_expired`: `cloud` (`aws`, `azure`, `gcp`, `oci`, `digitalocean`, `linode`), when
    the failure names its Terraform provider, and `reason`.
  - `address_pool_overlap`: `reason`.
  - `no_console`: `machine`.
  - `server_auth_failed`: `server`.
  - `io`: `path`, `action`.
  - `state_conflict`: `expected`, `found`.
  - `lease_held`: `scope`, `pid`.
- `detail` never holds a secret. The text a tool printed in it (a reason, a last line) is masked
  as `message` is. It is absent when the failure has no typed fields.

The Rust types are `isothrum_runner::ErrorReport`, `ErrorDetail` and `Detail`. The client hands
the report on as it is: `ClientError::Refused(report)` and `ClientError::Operation(report)`
(`ClientError::report()`).

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
