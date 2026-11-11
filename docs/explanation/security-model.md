# Security model

What Isothrum trusts, what it protects, and what it leaves to you. The vulnerability reporting
process is in [SECURITY.md](../../SECURITY.md).

## Trust boundaries

- **The run manifest and the generated files are trusted input.** Isothrum runs the commands a
  manifest lists with the user's rights. Running a manifest is like running a script: only run
  manifests generated from specs you trust. The manifest is checked against its schema before
  anything runs (`manifest_invalid`, `manifest_version`), but a valid manifest can still run any
  command.
- **The tools are trusted.** Docker, Vagrant, Terraform (and its providers), `ssh` and the cloud
  command lines run as the user who runs `isothrum`, with that user's access to them. Access to
  the Docker engine is equivalent to root on Linux.
- **The machines a run creates are not trusted with your secrets beyond what the manifest
  delivers to them.**

## Privileges

Isothrum has no daemon and no service. It runs the tools as the user who runs it and never asks
for more, with one exception:

- **Hyper-V on Windows.** Hyper-V accepts management commands only from an administrator or a
  member of the local *Hyper-V Administrators* group. For an operation that acts on a Hyper-V run
  (status reads excepted), Isothrum first checks the account's groups (`whoami /groups`). An
  elevated token or membership of *Hyper-V Administrators* runs the tools directly. Otherwise
  Isothrum starts one elevated broker for the operation (`Start-Process -Verb RunAs`, one UAC
  prompt, waited for up to 300 seconds) and sends each command to it through a fresh session
  folder in the run's state (`elevated/<token>/`); a request without the session's random token is
  deleted unrun. A declined or unanswered prompt, or a broker that stops before the operation ends, fails it
  with `elevation_failed`.
  An elevated process doesn't inherit the environment: the broker is handed `PATH`, the
  `VAGRANT_*` variables, the proxy and CA bundle variables, and each command's own values through
  an owner-only file it deletes once read (see [Environment variables](../reference/environment.md)).
  See [Use Hyper-V without administrator rights](../how-to/hyper-v-without-admin.md).

## Secrets

The manifest declares *slots* (secrets, inputs, connection credentials). Their values:

- **come from the environment only.** The CLI reads them with `--secret-from-env NAME[=VAR]`;
  there is no flag that takes a value on the command line. Params (`--param NAME=VALUE`) are not
  secret and are recorded.
- **never go on a command line.** A value reaches a tool only through the delivery the manifest
  names: `env` (the step's environment), `tf_var` (a `TF_VAR_*` variable), `state_file` (an
  owner-only Terraform variable file in the run's state folder) or `guest` (over SSH, on standard
  input, into one machine). When Terraform runs from a container image, its variables are passed
  by name (`docker run -e NAME`), not by value; anyone with access to the Docker engine can still
  read a running container's environment. The container also gets the host's proxy and CA bundle
  variables and `TF_CLI_CONFIG_FILE` by name, the files they name mounted read-only.
- **are masked.** Every sensitive value is replaced by `***` in events, log lines, error messages
  and results; a sensitive output shows as `***` in `status` too.
- **are not stored**, with five exceptions, all owner-only files in the run's state folder: a
  `state_file` delivery (written at `up`, kept for the destroy, removed by a successful `down`),
  a `guest` delivery's input file (removed once delivered, or when the operation is cut short; a
  removal that fails fails the step), an elevated Hyper-V session's `env-<n>.json`, a
  request's variables (deleted by the broker before it starts the program; the session's folder
  goes when the session ends), `consoles.json`, the port and password the runner chose for each
  machine's VMware VNC console (made before the run's first Vagrant command, removed by a
  successful `down`; VMware itself writes the password in the VM's `.vmx`), and
  `proxmox-console.curl`, a Proxmox console request's credentials, which `curl` reads on its
  standard input and the runner removes once it ran. The tools themselves may keep values: Terraform state can hold any
  value a resource received.

Terraform doesn't inherit the shell's cloud settings: `AWS_*`, `ARM_*`, `AZURE_*`, `GOOGLE_*`,
`CLOUDSDK_*`, `GCLOUD_*`, `DIGITALOCEAN_*`, `LINODE_*`, `OCI_*`, `TF_VAR_*` and `TF_CLI_ARGS*`
are removed from its environment. A run's cloud credentials and settings are the slots and params
given to the operation, so a `down` acts on the account the run was created in, not wherever the
shell points now.

## State on disk

Everything Isothrum keeps is under its home (`$ISOTHRUM_HOME`, else `~/.isothrum`); nothing is
written into the run's folder. See [State layout](../reference/state.md).

- On Unix, the folders Isothrum creates there (`runs/`, each run's folder, `ssh/`, the Terraform
  plugin cache) are `0700` and its files `0600`; an existing file or folder with wider permissions
  is narrowed when Isothrum writes it. A `TF_PLUGIN_CACHE_DIR` of your own is used as it is.
- On Windows, a home inside the user's profile (the default) keeps the profile's permissions; a
  home outside it gets a protected DACL (the account, SYSTEM, Administrators) that everything under
  it inherits.
- Removing a run's files never follows a symbolic link.

## SSH

The runner owns one key pair per home, `<home>/ssh/id_ed25519` (Ed25519, no passphrase, made on
first use). Each run gets its own `known_hosts` and `ssh_config` in its state folder: the runner's
key only, `StrictHostKeyChecking accept-new` (a machine's key is pinned the first time it is
seen), batch mode, and the user's `~/.ssh/config` is not read. Jump hosts get the same settings.
See [Manage the SSH key](../how-to/ssh-keys.md).

## Graphical consoles

`Running::graphical_console` hands an embedder a machine's console only on this computer's
loopback: the address the hypervisor gives is refused unless it is a loopback one, and a console
the manifest reaches through a machine of the run gets an SSH tunnel that listens on `127.0.0.1`
alone (`ssh -N -L 127.0.0.1:<port>:…`, `ExitOnForwardFailure`), with the run's key and pinned host
keys, open as long as the embedder holds the `Console`. Proxmox's console goes through its API's
websocket, with a ticket per session the embedder connects with. The passwords a `Console` holds
(the runner's for VMware, Proxmox's ticket, a Windows account's from a sensitive output) are
`SecretValue`s, never printed (`isothrum screen` prints one only with `--show-password`).
libvirt's, QEMU's and VirtualBox's consoles take no password: any program of a user of this
computer can reach them while the VM runs, as it can a forwarded port.

## Network

Isothrum opens no listening port on the network. It binds the loopback to find free ports for a
run's published ports. On macOS, a Vagrant run on VMware ESXi gets relays on the loopback to the
ESXi host (SSH, HTTPS, and WinRM through a forward proxy that only forwards WinRM's port); they
live for the operation. The runner also opens TCP connections itself for a manifest's `tcp`
ready conditions, and asks vagrant-qemu's monitor socket for a VM's VNC port. A graphical
console's SSH tunnel listens on the loopback only, while its `Console` lives. Everything else on the network is done by the tools.

## Supply chain

The release binaries are built by GitHub Actions from the tagged commit, with checksums, a
Sigstore signature and build provenance; see [Verify a release](../how-to/verify-a-release.md).
Dependencies are checked by `cargo deny` (advisories, licences, sources) on every change. The
crates forbid `unsafe` code.
