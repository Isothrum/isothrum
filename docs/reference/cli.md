<!-- Generated from the code: don't edit. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->

# The `isothrum` command line

isothrum: run what Isoloom generates (up, down, park, resume, status, check, events) from the command line.

```text
isothrum [OPTIONS] <COMMAND>
```

Every command takes the global options. Exit codes: [exit-codes.md](exit-codes.md); environment: [environment.md](environment.md).

## Global options

- `--home <DIR>`: The runner's home, where runs are recorded (default: `$ISOTHRUM_HOME`, else `~/.isothrum`)
- `--json`: Machine-readable output on standard output: an operation's events, and the ones `events` replays, one JSON object per line; `status`, `console`, `screen` and `key` print one JSON document; any failure is an `error` event. `completions` prints its script either way.

## `isothrum up`

Start the run (a parked run is resumed; leftovers are removed first)

```text
isothrum up [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--replace`: Remove whatever of the run exists, then start afresh.
- `--lifetime <MINUTES>`: How long a run that stops itself (a cloud run) may live, in minutes (default: its manifest's)
- `--ssh-jump <USER@HOST>`: A login (`user@host`) the run's SSH goes through first: a Proxmox node whose run bridge this machine can't route to. Recorded when the run starts: every later operation (a resume through `up` too) goes the same way.

## `isothrum down`

Remove everything the run created.

```text
isothrum down [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum park`

Stop the run, keeping what it created.

```text
isothrum park [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--mode <MODE>`: `pause` or `shutdown`. One of `pause`, `shutdown`. Default: `shutdown`.

## `isothrum resume`

Start a parked run again.

```text
isothrum resume [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum update`

Bring a running run up to date with its manifest, regenerated since it started (a machine added or taken out): its start steps again, with the params it was started with.

```text
isothrum update [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum retire`

Take a machine out of the running run, ahead of its removal from the manifest and the `update` after.

What the update wouldn't remove itself: a Vagrant VM is destroyed, a Compose service stopped and removed (also one only moved out of the run's start).

```text
isothrum retire [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine, by its manifest name.
- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum status`

Show the run: its state, machines, outputs and message.

```text
isothrum status [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)

## `isothrum check`

Run the run's checks (exit 1 when one fails)

```text
isothrum check [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.

## `isothrum provision`

Run the set-up again on the running machines (a VM run's provisioners)

```text
isothrum provision [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--machine <MACHINE>`: Only this machine.

## `isothrum attach`

Hot-add the run's on-demand access machine into the running run.

```text
isothrum attach [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--machine <MACHINE>`: Which on-demand machine, when the run has more than one (default: its only one)
- `--network <NETWORK>`: The run's network to attach it to (default: its first)
- `--image <IMAGE>`: What to run it from (default: the run's): a container image for a Compose access service, the Vagrant box an access VM boots.

## `isothrum detach`

Hot-remove the on-demand access machine from the running run.

```text
isothrum detach [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)
- `--param <NAME=VALUE>`: A param's value (repeatable)
- `--secret-from-env <NAME[=VAR]>`: A secret or input slot's value, read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.
- `--host-label <LABEL>`: What to call the place the run is on (recorded; a later operation elsewhere is refused)
- `--provider <PROVIDER>`: The Vagrant provider a VM run runs on, one of the manifest's; a run keeps the one it was started with. One of `virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `utm`, `vmware_esxi`.
- `--machine <MACHINE>`: Which on-demand machine, when the run has more than one (default: its only one)

## `isothrum console`

Print the command that opens an interactive console into any machine of the run.

```text
isothrum console [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine, by its manifest name.
- `--user <USER>`: The account to open it as, in its home, rather than the one the way in logs in as (a container's own, Vagrant's `vagrant`, a module's SSH user): the machine must have it, and on a VM that login must be allowed to `sudo`. Not on a Windows machine.
- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)

## `isothrum screen`

Reach a machine's graphical console (its screen) and print where it answers: an address of this computer's loopback for a VNC or RDP viewer, or Proxmox's VNC websocket. One reached through an SSH tunnel is held open until the command is stopped (Ctrl-C)

```text
isothrum screen [OPTIONS] <MACHINE> [PATH]
```

- `<MACHINE>`: The machine, by its manifest name.
- `--show-password`: Print its password too (the VNC password, or the account's for RDP)
- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)

## `isothrum events`

Print the run's recorded events, oldest first.

```text
isothrum events [OPTIONS] [PATH]
```

- `<PATH>`: The run's folder (the manifest Isoloom generated under its `.isoloom/`, or an instance's `.isoloom-<n>/`, is read), or a run.json. Default: `.`.
- `--target <TARGET>`: The target whose manifest to run (`docker`, `cloud-vm/aws`, an instance's `.isoloom-2/docker`), when the run was generated for several.
- `--name <NAME>`: The name to run it under, so one manifest can run several times side by side (default: the environment's name in the manifest)

## `isothrum reap`

Destroy a cloud's runs past their expiry (each with the params it was applied with), then list what the cloud still holds past its expiry that no run records. Exit 1 when a destroy failed, something is left or the cloud couldn't be searched in full.

```text
isothrum reap [OPTIONS] <CLOUD>
```

- `<CLOUD>`: The cloud. One of `aws`, `azure`, `gcp`, `digitalocean`, `linode`, `oci`.
- `--dry-run`: Say what would be destroyed, destroying nothing.
- `--region <REGION>`: An AWS region to look for leftovers in (repeatable; default: the AWS command line's)
- `--project <PROJECT>`: The Google Cloud project to look for leftovers in (default: gcloud's)
- `--secret-from-env <NAME[=VAR]>`: A connection slot's value (the account's keys), read from an environment variable (default: the slot's own name). Repeatable. Values never go on the command line.

## `isothrum key`

Print the runner's SSH public key (made when it has none): authorize it where a run's SSH goes through (a Proxmox node, with `--ssh-jump`). With `--json`, `{"public_key": …}`.

```text
isothrum key [OPTIONS]
```


## `isothrum completions`

Print the shell completion script for `shell` (e.g. `isothrum completions bash > /etc/bash_completion.d/isothrum`)

```text
isothrum completions [OPTIONS] <SHELL>
```

- `<SHELL>`: The shell. One of `bash`, `elvish`, `fish`, `powershell`, `zsh`.
