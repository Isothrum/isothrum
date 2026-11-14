# Terraform modules

Isothrum runs any Terraform module: a folder that holds `*.tf` or `*.tf.json` files. It needs no
output, variable or file of its own. What it reads comes from Terraform itself: `show -json` for the
state, and `output -json` for the outputs. Isoloom's modules follow the conventions below, but any
module may follow them too, and a module that follows none still runs.

## What every module gets

| | |
|---|---|
| Lifecycle | `up` and `update`: `init`, then `apply`. `down`: `destroy`. `park`, `resume`, `provision`, `attach`, `retire`, `exec` and `console` are refused (a module has no stopped state, no machine of its own to reach). |
| State | The run's own, in its state folder (`-state=`, `TF_DATA_DIR`), unless the module names a backend. An existing deployment's local state can be adopted (`state.md`). |
| Values | Each value and secret a run is given reaches the module's variable of the same name as `TF_VAR_<name>`, and the providers' own variables (`AWS_ACCESS_KEY_ID`, `PROXMOX_VE_ENDPOINT`) as they are. Values are recorded and given again at every later operation. Secrets never are: a `down` or a reap that needs one is given it again. |
| Variables | `Project::variables`, `Client::variables` and the service's `variables` command list the module's `variable` blocks: name, type, default, sensitive and description. Give a sensitive one as a secret. |
| Outputs | Read after every `up` (in its result) and every status. Sensitive ones show as `***`. |
| Machines | Each managed instance of a VM resource type (`aws_instance`, `proxmox_virtual_environment_vm`, `esxi_guest` and the other clouds' and hypervisors' own) is a machine named after its resource. A state that holds data sources only is a ready run with no machine. |
| Resources | Every managed instance is recorded by address and id, so that a run killed mid-apply is still removed, by id, at the next operation. |
| Lifetime | `--lifetime` and `RunOptions::lifetime` record when the run expires. The service's reaper, and `isothrum reap`, then run its `down`. |

## Optional conventions

A module may give these outputs, and a run may give these values. Each one adds a feature. None
is required.

| Name | Kind | What it gives |
|---|---|---|
| `consoles` | output, a map | The machines with a screen, each with its protocol (`{ dc = "rdp" }`), or with more fields (`{ dc = { protocol = "vnc", port = 5901, host = "10.9.0.20", via = "203.0.113.7", user = "admin" } }`). No other machine has a screen. |
| `windows_password` | output, sensitive | Without `consoles`, each Windows VM gets Remote Desktop with this password. Also read from the run's own value or secret of that name. |
| `windows_user` | output | Remote Desktop's user, when it isn't `Administrator`. |
| `vnc_password` | output, sensitive | A VNC console's password. |
| `ip` | output | The machine a tunnelled screen goes through: an access machine, a router, the first Linux VM. |
| `ssh_user`, `ssh_user_<machine>`, `address_<machine>` | outputs | Who signs in there: the user of the machine whose address is `ip`, else `ssh_user`. |
| `ssh_private_key_file` | run value | The private key the tunnel signs in with. Without it, the runner's own key is used. |
| `proxmox_endpoint` | output | The Proxmox VE server, for a module that takes it as a variable instead of from `PROXMOX_VE_ENDPOINT` or `PM_API_URL`. |
| `PROXMOX_VE_*`, `PM_*` | run values and secrets | Proxmox VE's API, as bpg/proxmox and Telmate/proxmox read it. With these, a Proxmox VM's screen (`vncproxy`), its web console, and its state on the server (removed or powered off in Proxmox's own UI) are read through the API. |
