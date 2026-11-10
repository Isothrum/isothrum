# State layout

Isothrum keeps everything under one folder, its **home**: `--home DIR`, else `$ISOTHRUM_HOME`,
else `~/.isothrum`. It writes nothing into the run's folder (the one holding `.isoloom/`).

```text
<home>/
  runs/
    <run>/                 one folder per run, named after the run
      record.json          the run's record
      lock                 the OS lock: one operation per run at a time, across processes
      events.jsonl         the last operations' events (trimmed to half past 4000 lines)
      ...                  the run's state folder (${run.state_dir}): what the manifest keeps there
  slots.lock               the OS lock a start holds while it picks its slot (${run.slot})
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

`<home>/runs/<run>/` is also the run's `${run.state_dir}`: what the generated manifest keeps there
depends on the target. With Isoloom's current targets:

| Target | What it keeps |
|---|---|
| Compose | `ports.yml`, the host ports pinned for the run. |
| Vagrant | `vagrant/`, Vagrant's own state (`VAGRANT_DOTFILE_PATH`), and the host ports the run forwards (in the record). |
| Terraform | `terraform.tfstate` and its backup, `.terraform/` (`TF_DATA_DIR`), a `state_file` delivery's variable file while the run exists, and `module/`, the copy of the run's own module a `terraform.copy_module` step makes. |
| Any, over SSH | `ssh_config` and `known_hosts` for the run; `guest-<machine>.in`, a guest delivery's values, only while it is delivered. |
| Any, with `project.archive` | `project.tgz`, the project as a gzipped tar for a later step to pipe into a guest: what the generated files' copies leave out stays out (`.isoloomignore`, Isoloom's outputs but those `with.outputs` names, version control and the tools' state such as a Terraform state). |
| Hyper-V on Windows | `elevated/<token>/`, the elevated broker's session folder, one per operation. |

**Terraform state is local**: it lives in the run's state folder under the home, not in a remote
backend. A `down`, `update` or `reap` needs the same home the `up` used. See
[Use isothrum in CI](../how-to/ci.md).

## `record.json`

The run's record, written atomically. It holds no secret value.

| Field | Meaning |
|---|---|
| `version` | The record format's version: `0`. |
| `name` | The run's name. |
| `manifest` | The absolute path of the manifest the run was last brought up or updated from. |
| `target`, `driver` | The manifest's target and driver. |
| `cloud` | The cloud, for a cloud run. |
| `provider` | The Vagrant provider, for a Vagrant run. |
| `host_label` | The place the run is on, when one was given (`--host-label`). |
| `state` | Where the run stood after its last operation (`ready`, `parked`, `absent`, ...). |
| `params` | Every param the run was started with; later operations replay them. |
| `vm_host_ports` | The host ports a local VM run forwards, by `<machine>/<port>`. |
| `vm_boot_digests` | Each VM's boot digest, which `update` compares to restart a VM whose boot-time settings changed. |
| `started_at` | When it was last started. |
| `slot` | Its `${run.slot}` (1 to 99), when the manifest uses one. |
| `ssh_jump` | The jump host given at `up` (`--ssh-jump`). |
| `expires_at` | When the run expires, in seconds since the Unix epoch, for a manifest with an expiry. |
| `last` | The last operation: `op`, `ok`, the error `code` when it failed, and `at`. |

Fields that don't apply are left out. The record stays after a successful `down`, with state
`absent`. How the format is versioned is in [Policies](../policies.md#what-is-stable).

## Moving or backing up

The home can be copied or moved while no operation runs; see
[Move the home](../how-to/move-the-home.md).
