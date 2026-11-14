<!-- Generated from the code: don't edit. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->

# The `isothrum` command line

isothrum: run a Compose project, a Vagrantfile or a Terraform module from the command line.

```text
isothrum [OPTIONS] <COMMAND>
```

Every command takes the global options. Exit codes: [exit-codes.md](exit-codes.md); environment: [environment.md](environment.md).

## Global options

- `--home <DIR>`: The runner's home, where runs are recorded (default: `$ISOTHRUM_HOME`, else `~/.isothrum`)
- `--json`: Machine-readable output on standard output: an operation's events, and the ones `events` replays, one JSON object per line; `status`, `console` and `key` print one JSON document; any failure is an `error` event. `completions` prints its script either way.
- `--no-service`: Run the command in this process rather than through the home's runner service (also `ISOTHRUM_NO_SERVICE=1`): for tests and CI, where nothing should outlive the command.

## `isothrum up`

Start the run, and wait until the project says it's ready.

```text
isothrum up [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--lifetime <MINUTES>`: How long the run may live, in minutes, 1 or more: recorded, and `reap` removes it after.

## `isothrum down`

Remove everything the run created.

```text
isothrum down [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum park`

Stop the run, keeping what it created.

```text
isothrum park [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--mode <MODE>`: `pause` or `shutdown`. One of `pause`, `shutdown`. Default: `shutdown`.

## `isothrum resume`

Start a parked run again.

```text
isothrum resume [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum update`

Bring a running run to what the project's files now say.

```text
isothrum update [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum retire`

Take a machine out of the running run (a Vagrant machine destroyed, a Compose service stopped and removed)

```text
isothrum retire [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine.
- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum status`

Show the run: its state, its machines and the ports they publish, and its outputs.

```text
isothrum status [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)

## `isothrum exec`

Run a command in a machine of the running run, with no terminal (`exec web -- id -u`): what it prints, and its exit status as the command's.

```text
isothrum exec [OPTIONS] <MACHINE> [PATH] -- <ARGV>...
```

- `<MACHINE>`: The machine (a Compose service, a Vagrant machine)
- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `<ARGV>`: The command and its arguments, after `--`.

## `isothrum provision`

Run the set-up again on the running machines (a Vagrant project's provisioners)

```text
isothrum provision [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--machine <MACHINE>`: Only this machine.
- `--with <WITH>`: Only the provisioner of this name (Vagrant's `--provision-with`, a lab's `checks`)

## `isothrum attach`

Start a machine the project leaves down at `up` (a Compose service under a profile, a Vagrant machine with `autostart: false`)

```text
isothrum attach [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine.
- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum console`

Print the command that opens an interactive console into a machine of the run.

```text
isothrum console [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine (a Compose service, a Vagrant machine, a module's VM)
- `--user <USER>`: The account to open it as, in its home, rather than the one the way in logs in as (a container's own, Vagrant's `vagrant`): the machine must have it, and on a VM that login must be allowed to `sudo`.
- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)

## `isothrum screen`

Reach a machine's graphical console (its screen) and print where it answers: an address of this computer's loopback for a VNC or RDP viewer, or Proxmox's VNC websocket. A module's Windows VM's Remote Desktop is reached through an SSH tunnel to the Linux machine beside it, open until Ctrl-C. With `--app`, what opens it in its hypervisor's own app instead.

```text
isothrum screen [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine (a Vagrant machine, a module's VM)
- `--show-password`: Print its password too (Proxmox's ticket, a Windows VM's administrator password)
- `--app`: Print what opens it in its hypervisor's own app (a program and its arguments, a macOS app, a URL), best first.
- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)
- `--env <NAME=VALUE>`: A value the project's tools get (`NAME=VALUE`, repeatable): Compose's `${NAME}`, a Vagrantfile's `ENV["NAME"]`, a Terraform variable `NAME`.
- `--secret-from-env <NAME[=VAR]>`: A secret the project's tools get as a value is, read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded)
- `--provider <PROVIDER>`: The Vagrant provider a Vagrant project runs on. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum events`

Print the run's recorded events, oldest first.

```text
isothrum events [OPTIONS] [PATH]
```

- `<PATH>`: The project's folder (a file in it names its folder) Default: `.`.
- `--format <FORMAT>`: The project's format, when its folder holds the files of several. One of `compose`, `vagrant`, `terraform`.
- `--name <NAME>`: The name to run it under, so one project can run several times side by side (default: the folder's name)

## `isothrum reap`

Remove the recorded runs past their lifetime (`up --lifetime`), each from the folder it was started from. Exit 1 when a removal failed, or one is left (`--dry-run`)

```text
isothrum reap [OPTIONS]
```

- `--dry-run`: Say what would be removed, removing nothing.
- `--env <NAME=VALUE>`: A value every removal's tools get (`NAME=VALUE`, repeatable): an account's region, say.
- `--secret-from-env <NAME[=VAR]>`: A secret every removal's tools get (the account's keys), read from an environment variable (default: the secret's own name). Repeatable. Values never go on the command line.

## `isothrum key`

Print the runner's SSH public key (made when it has none): authorize it where a run's tools reach a machine as the runner. With `--json`, `{"public_key": …}`.

```text
isothrum key [OPTIONS]
```


## `isothrum serve`

Run the home's runner service: it runs every operation of the home (one at a time per run, runs side by side) and streams their events to its clients. The other commands start it when it isn't running and talk to it (unless `--no-service`)

```text
isothrum serve [OPTIONS]
```

- `--idle-timeout <SECONDS>`: Stop after this many seconds with no client and no operation (0: never) Default: `600`.
- `--detach`: Run as a background service, as a client starts it: a terminal's hang-up doesn't stop it, and it logs at `info` unless `ISOTHRUM_LOG` says otherwise.

## `isothrum completions`

Print the shell completion script for `shell` (e.g. `isothrum completions bash > /etc/bash_completion.d/isothrum`)

```text
isothrum completions [OPTIONS] <SHELL>
```

- `<SHELL>`: The shell. One of `bash`, `elvish`, `fish`, `powershell`, `zsh`.
