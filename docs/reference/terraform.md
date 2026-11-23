# Terraform modules

Isothrum runs any Terraform module: a folder that holds `*.tf` or `*.tf.json` files. It needs no
output, variable or file of its own. What it reads comes from Terraform itself: `show -json` for the
state, and `output -json` for the outputs. A module may follow the conventions below, and a module
that follows none still runs.

## What every module gets

| | |
|---|---|
| Lifecycle | `up` and `update`: `init`, then `apply`. `down`: `destroy`. `park`, `resume`, `provision`, `attach` and `retire` are refused (a module has no stopped state, and its machines are made and removed by its apply only). |
| Console and commands | `console <machine>` (`Running::console_command`) and `exec <machine>` (`Running::exec`, no terminal) are an `ssh` into the VM, along its route: to its `address_<machine>` output, else to the address the `machines` output names, through the `ip` output's machine (unless it is at that address), else to its address in the state; through the run's `ssh_jump` value first when it has one. Signed in as `ssh_user_<machine>`, else the `ssh_user` of the machine whose `address_<machine>` it is, else `ssh_user` (a console asked for another account then opens a login shell as it, `sudo -iu`); with the run's `ssh_private_key_file` value, else the runner's own key, at every hop. The first host on the way is pinned before, in the run's own known hosts (the jump host is checked against the run's `ssh_jump_known_hosts_file` instead, when given); a host behind it, which only it reaches, is pinned the first time a call meets it. A command that runs and fails is its exit status; one `ssh` can't run (it ends with 255) is `tool_failed`. A Windows VM has neither: its screen is its Remote Desktop. |
| Service endpoints | `Running::service_endpoints` gives each entry of the `published` output (`{ "web/site" = "192.0.2.10:8080" }`) where this computer reaches it: as the module gives it, or, when the run has an `ssh_jump` value (its network is internal to that host), at a local forward through the jump host (`ssh -N -L 127.0.0.1:<port>:<host>:<port>`, the run's key, the jump host's key pinned first, or checked against `ssh_jump_known_hosts_file`). This process keeps a forward while the run publishes its endpoint, opens it again when its `ssh` ended, and ends it when the endpoint is gone; a `down` ends it from any process, through the run's tool ledger. One that doesn't open says why. |
| State | The run's own, in its state folder (`-state=`, `TF_DATA_DIR`), unless the module names a backend. An existing deployment's local state can be adopted (`state.md`). |
| Values | Each value and secret a run is given reaches the module's variable of the same name as `TF_VAR_<name>`, and the providers' own variables (`AWS_ACCESS_KEY_ID`, `PROXMOX_VE_ENDPOINT`) as they are. Values are recorded and given again at every later operation. Secrets never are: a `down` or a reap that needs one is given it again. |
| Variables | `Project::variables`, `Client::variables` and the service's `variables` command list the module's `variable` blocks: name, type, default, sensitive and description. Give a sensitive one as a secret. |
| Outputs | Read after every `up` (in its result) and every status. Sensitive ones show as `***`. |
| Machines | Each managed instance of a VM resource type (`aws_instance`, `proxmox_virtual_environment_vm`, `esxi_guest` and the other clouds' and hypervisors' own) is a machine named after its resource, at the address its provider writes in the state, else the one the `machines` output names. A state that holds data sources only is a ready run with no machine. |
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
| `published` | output, a map | The deployment's service endpoints, by name, at `host:port` (`{ "web/site" = "192.0.2.10:8080" }`): `Running::service_endpoints`. |
| `machines` | output, a map | Each machine's address on the module's own networks (`{ web = "10.60.0.10" }`): a machine the state gives none takes it (a Proxmox VE VM without a guest agent), and a console or a command reaches it through the `ip` output's machine. |
| `ip` | output | The machine a tunnelled screen, and a console or a command to a `machines` address, goes through: an access machine, a router, the first Linux VM. |
| `ssh_user`, `ssh_user_<machine>`, `address_<machine>` | outputs | Who signs in there: the user of the machine whose address is `ip`, else `ssh_user`. |
| `ssh_private_key_file` | run value | The private key a tunnel, a console and a command sign in with. Without it, the runner's own key is used. |
| `ssh_jump` | run value | `user@host`: the host every console, command and service endpoint of the run goes through first, one this computer reaches that reaches the module's machines (a host whose private network only it reaches, such as a hypervisor's node with an internal bridge). Signed in to with the same key. |
| `ssh_jump_key_file` | run value | The private key that signs in to the `ssh_jump` host for the module's own SSH (below); when not given, the run's `ssh_private_key_file` value, else the runner's own key, as for every hop. |
| `ssh_jump_known_hosts_file` | run value | A `known_hosts` file of the `ssh_jump` host's keys (ones its user confirmed, say): every SSH to that host (the proxy below, a console's or a command's first hop, a service endpoint's forward) checks it there strictly (`StrictHostKeyChecking=yes`), and only reads the file; the host is then never pinned in the run's own known hosts. A key the file doesn't hold is refused in `ssh`'s words; a file that isn't there fails the step (`tool_failed`, step `host-key`). When not given, the host is pinned the first time the run meets it. |
| `ssh_proxy_host`, `ssh_proxy_port` | module variables | Set for each `apply` and `destroy` of a run with an `ssh_jump` value: a SOCKS proxy through the jump host on this computer's loopback (`ssh -N -D 127.0.0.1:<port>`, the jump host's key pinned first, or checked against `ssh_jump_known_hosts_file`), which the module's provisioners' SSH goes through to reach machines only the jump host reaches. The proxy is a tool of the step: it ends with it, and the run's tool ledger ends it after a crash. One that doesn't open, or ends first, fails the step (`tool_failed`, step `jump`) in `ssh`'s words. |
| `proxmox_endpoint` | output | The Proxmox VE server, for a module that takes it as a variable instead of from `PROXMOX_VE_ENDPOINT` or `PM_API_URL`. |
| `PROXMOX_VE_*`, `PM_*` | run values and secrets | Proxmox VE's API, as bpg/proxmox and Telmate/proxmox read it. With these, a Proxmox VM's screen (`vncproxy`), its web console, and its state on the server (removed or powered off in Proxmox's own UI) are read through the API. |
| `PROXMOX_VE_TLS_FINGERPRINT` | run value | The SHA-256 fingerprint of the server's certificate (64 hex digits, colons optional, as Proxmox VE's web interface shows it): the runner's own API requests trust that certificate alone. Without it, the system's trust store checks the certificate; the providers' `PROXMOX_VE_INSECURE` and `PM_TLS_INSECURE` are theirs only. |
