# State layout

Isothrum keeps everything under one folder, its **home**: `--home DIR`, else `$ISOTHRUM_HOME`,
else `~/.isothrum`. It writes nothing into the run's folder (the project's own).

```text
<home>/
  state.db                 the home's store (SQLite, WAL: with state.db-wal and state.db-shm):
                           the runs, the journal of operations, the events, the resources each
                           run made, what the runs' downloads fetched, the host ports reserved
                           and the leases (below)
  runs/
    <run>/                 one folder per run, named after the run
      lock                 the OS lock: one operation per run at a time, across processes
      ...                  the run's state folder (${run.state_dir}): what the manifest keeps there
  slots.lock               the OS lock a start holds while it picks its slot (${run.slot})
  service.lock             the OS lock the runner service holds while it runs: one per home
  service.json             what the running service says of itself (pid, start, release,
                           protocol, endpoint); removed when it stops
  service.sock             its endpoint on Unix (on Windows: a named pipe, named in service.json)
  service.log              what a service a client started writes (rolled over past 8 MiB)
  service.replace.lock     the OS lock a client holds while it replaces a service of another
                           release ([Runner service](service.md#replacing-a-service-of-another-release))
  ssh/
    id_ed25519             the runner's SSH key, and id_ed25519.pub (made on first use)
  terraform-plugins/       the Terraform provider cache every run of this home shares,
                           unless TF_PLUGIN_CACHE_DIR names another one
  relay/
    ovftool                on macOS, a Vagrant run on VMware ESXi: the wrapper that sends
                           ovftool's upload through the run's loopback relay
```

On Unix, the folders Isothrum creates are `0700` and its files `0600`; on Windows, a home outside
the user's profile gets an owner-only DACL
([Security model](../explanation/security-model.md#state-on-disk)).

## The run's state folder

`<home>/runs/<run>/` is the run's state folder. What it keeps depends on the project's format:

| Format | What it keeps |
|---|---|
| Compose | `ports.yml`, the host ports pinned for the run. |
| Vagrant | `vagrant/`, Vagrant's own state (`VAGRANT_DOTFILE_PATH`), and the host ports the run forwards (in the record); `consoles.json`, the loopback port and password the runner chose for each VM's VMware VNC console, until `down`. |
| Terraform | `terraform.tfstate` and its backup, `.terraform/` (`TF_DATA_DIR`), a `state_file` delivery's variable file while the run exists, and `module/`, the copy of the run's own module a `terraform.copy_module` step makes. |
| Any, over SSH | `ssh_config` and `known_hosts` for the run; `guest-<machine>-<random>.in`, a guest delivery's values, one file per delivery, only while it is delivered. |
| Proxmox | `proxmox-console-<random>.curl`, the credentials of an API request (a file per request) (a console's `vncproxy`, a status read asking where the VMs stand), only while `curl` reads them: removed however the request ends (a cancel, Ctrl-C). |
| Any | `tools/`, the tools an operation runs, while they run: `<token>-<pid>.tool` each (its process id and start), and `<token>.owner`, which the process that started them holds locked while it lives. The next operation ends the tools of a process that died (killed, crashed) before it acts, and removes their entries. |
| Hyper-V on Windows | `elevated/<token>/`, the elevated broker's session folder, one per operation. |

**Terraform state is local**: it lives in the run's state folder under the home, not in a remote
backend. A `down`, `update` or `reap` needs the same home the `up` used. See
[Use isothrum in CI](../how-to/ci.md).

**A resource already gone leaves the state**: when a `down`'s `terraform destroy` fails with an
error the manifest's `identity.terraform.gone` names, filled with the attributes of an instance
the state still holds (bpg/proxmox's `no such IPSet 'iso1n0'` for an IP set a reset Proxmox host
lost), the runner drops that instance with `terraform state rm`, run as the destroy is, emits an
`already_gone` warning naming it, and destroys again (three times at most). No other instance is
dropped.

## `state.db`

The home's store, one SQLite database, every change one transaction. It holds no secret value.

| Table | What it holds |
|---|---|
| `runs` | Each run: what the user wants of it (`desired`: `running`, `parked`, `gone`), where it stands (`state`, and `failed_cause` for a failed one), the operation that put it there, its record (the fields below), and what the runner last saw of it (its machines and where their ports answer, never an output's value: what `status` shows while an operation holds the run). |
| `transitions` | Every move of a run's state, with the operation that made it (none for a status read's). The moves are a closed table: an operation that can't begin from where the run stands is refused (`illegal_transition`). |
| `operations`, `operation_steps` | The journal: each operation's start, its steps, and its end (`succeeded`, `failed`, `cancelled`, `abandoned`). An operation with no end is one whose process died midway (killed, crashed, the computer off): the next operation on the run, once it holds the run, ends it `abandoned` and records a run it left moving (`creating`, `destroying`, `parking`, `resuming`, `updating`) as `failed` (`cancelled`), so the next `up` clears what it left. |
| `events` | The runs' events, in order; a run's are trimmed to the newest 2000 once over 4000, by the operation holding the run. |
| `resources` | What each run made or keeps, written ahead: `creating` before the call that makes it, `created` with the tool's id after, `destroying` before the call that removes it, `gone` after. Provider resources (a Compose run's anonymous volumes today) and the runner's own local things (state files, pinned host keys, the project archive, consoles). A resource an operation whose process died left behind is removed by the id the tool gave it, never by a name. Each move is a `resource` event. |
| `downloads` | What the runs' own downloads fetched: each image a Compose start's pull pulled (its reference, the id it stood for once pulled, the Docker context it went through) and each box a Vagrant `up` added (name, provider, version, architecture), with the run and when. An image counts only when its reference stands, after the pull, for an id the engine didn't hold before it; a box only when `vagrant box list` lists it after the `up` and didn't before. What was there before, or what anything else fetched, is never recorded. A row is forgotten once its download is removed or found gone ([Runner service](service.md#everything-else-a-client-asks-of-a-home)). |
| `port_reservations`, `leases` | The host ports a start reserved, held under its run's lease, which its process renews while it holds them: a process that dies stops renewing it, and the next start takes the port over. |

The record's fields:

| Field | Meaning |
|---|---|
| `name` | The run's name. |
| `manifest` | The absolute path of the manifest the run was last brought up or updated from. |
| `target`, `driver` | The manifest's target and driver. |
| `cloud` | The cloud, for a cloud run. |
| `provider` | The Vagrant provider, for a Vagrant run. |
| `host_label` | The place the run is on, when one was given (`--host-label`). |
| `params` | Every param the run was started with; later operations replay them, a param the record lacks taking the value given, else its default (a required one with neither is refused). |
| `vm_host_ports` | The host ports a local VM run forwards, by `<machine>/<port>`. |
| `vm_boot_digests` | Each VM's boot digest, which `update` compares to restart a VM whose boot-time settings changed. |
| `started_at` | When it was last started. |
| `slot` | Its `${run.slot}` (1 to 99), when the manifest uses one. |
| `ssh_jump` | The jump host given at `up` (`--ssh-jump`). |
| `expires_at` | When the run expires, in seconds since the Unix epoch, for a manifest with an expiry. |
| `last` | The last operation: `op`, `ok`, the error `code` when it failed, and `at`. |

A run's row stays after a successful `down`, with state `absent`. A store a newer Isothrum wrote
is refused (`store_version`).

### When `state.db` is damaged

The store is kept the way a journaling file system keeps a volume. Every change is one SQLite
transaction in WAL mode, so a process killed mid-change (kill -9, a crash, a power cut) leaves no
trace of it. Every open runs `PRAGMA quick_check` first.

Beside the store, `state.mirror` holds a copy of the rows a later operation can't do without:
- every run, but its last observation;
- every live resource with the tool's id for it;
- the operations those refer to, and every open one;
- what the runs' downloads fetched.

It is rewritten whole (temporary file, `fsync`, rename) by each transaction that changes those
rows, and only by those.

When `state.db` is damaged (it fails its check, isn't a database, or is empty beside a mirror that
holds rows), the runner rebuilds it from the mirror:
- The damaged file and its `-wal` and `-shm` are moved to `state.db.corrupt-<unix seconds>` for
  whoever looks at them.
- Its event cursor moves past the old one.
- A repaired run's open operation is recovered by its next operation, and what it made is removed
  by recorded id.
- Each run's state is reconciled with what its tools report at its next `status`.

The history (events, steps, state moves) is what a repair loses. With no mirror to rebuild from,
opening fails with `store_corrupt`, naming what the check found.

## Upgrading from 2.x

3.0 doesn't read a home a 2.x runner wrote. Before upgrading, stop and remove every 2.x run with
the 2.x command line (`isothrum down` for each). A 3.0 home starts empty: the files a 2.x runner
left in it (`runs/<run>/record.json`, `seen.json`, `events.jsonl`,
`anonymous-volumes`, `ports/`) are neither read nor removed, and can be deleted by hand.

## Adopting an existing Terraform deployment

An app embedding the runner can adopt a Terraform deployment applied from a run manifest's module
outside the runner (by hand, by another tool, from a home that is gone) as a run of that manifest:
`Runner::adopt` with an `Adoption` (the run's name, the folder holding the deployment's local
`terraform.tfstate`, and what the state doesn't say: the params it was applied with, its place,
its expiry and start time). The state, and its `.backup`, move into the run's state folder; the
old folder keeps no copy, and its `.terraform/` stays behind (`init` makes a new one). The run is
recorded as `ready` with those params, so `status`, `update`, `down` and `reap` act on it like any
other run, a `down` destroying it where it was applied.

Adopting is refused (`adopt_refused`) for a manifest that isn't a Terraform one, a run whose
record isn't `absent`, a run that already has a state, a folder with no state, or a slot
(`${run.slot}`, the one the deployment was applied in) out of 1 to 99 or held by another run on
its place. The slot is picked and recorded under `slots.lock`, as a start's is; with none given,
the run takes the lowest free one. A state in a
remote backend is pulled into a local file first (`terraform state pull > terraform.tfstate`).

## Moving or backing up

The home can be copied or moved while no operation runs; see
[Move the home](../how-to/move-the-home.md).
