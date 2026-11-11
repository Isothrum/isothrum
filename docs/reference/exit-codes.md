<!-- Generated from the code: don't edit. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->

# Exit codes and error codes

## Exit codes

| Code | When |
| ---- | ---- |
| 0 | The command reached its goal: the run is up (or parked, down, …), every check passed. |
| 1 | It didn't: an operation failed (its `error` event says why, with its code), a check failed, or the runner refused (a run that is busy, a manifest it can't read). |
| 2 | A usage error: an unknown option, a value missing or malformed, a run folder with no manifest (the `error` event's code is `usage`). |
| 130 | SIGINT (Ctrl-C) cancelled it: the tools it started were asked to stop (SIGINT) and ended once they had, 30 s later or on a second signal (at once on Windows), and the operation ended with an `error` event, code `cancelled` (on Windows: Ctrl-C). |
| 143 | SIGTERM cancelled it (a CI runner cancelling its job), as for 130 (on Windows: Ctrl-Break, the console closed, a log-off or a shutdown). |

## Error codes

An operation that fails ends its event stream with an `error` event whose `code` is one of these (with `--json`, also a failure before any operation began). The code is stable; the message is for a person.

| Code | Cause | Remedy |
| ---- | ----- | ------ |
| `manifest_invalid` | The manifest isn't valid JSON or breaks the contract, a value it gives can't be used as written (a step's `verdict` that isn't a regular expression, an SSH login, a `wait_s`, a `tcp` condition's port), or it names a value the operation doesn't have (a placeholder with none, a guest delivery's machine whose address no output gives, a Terraform module with no `.tf` file). | Generate the run's files again with the Isoloom this runner was built for; a value the message names is the run's own to fix. |
| `manifest_version` | The manifest is another version of the contract. | Use the Isothrum release that reads this manifest version, or regenerate the run with a matching Isoloom. |
| `unsupported` | The manifest needs something this runner doesn't do (a target that doesn't run on this host, a delivery its driver doesn't take, an action of another driver's, a step with no command for this host), or the call asks the run for something its target can't do (a console into a Compose service with no VM, a Vagrant VM joined to another network). | Upgrade Isothrum, generate the run for another target, or ask the run for what its target does. |
| `io` | A file or folder couldn't be read or written, or a socket on this computer bound. | Check the path in the message: free space, permissions, the runner's home (`--home`, `ISOTHRUM_HOME`); for a port, the open-file limit (`ulimit -n`) and the loopback. |
| `locked` | Another process runs an operation on this run, or holds the home's slot pick. | Wait for the other operation on the run (or the other start's slot pick) to end, or stop the process running it. |
| `tool_missing` | A tool a step runs isn't installed. | Install the tool, or put it on `PATH`. |
| `tool_failed` | A step's tool ran and failed. | Read the step's last lines (its `log` events, `isothrum events`): fix what the tool says, then run the command again. |
| `tool_too_old` | A tool answers at a version older than the run needs. | Upgrade the tool to the version the message names. |
| `timeout` | A step showed no progress for its time limit, or ran to the ceiling every step stops at, and was stopped, with what it started (the `error` event's `timeout` says which). | See the event's `timeout`: a step stuck with no output, or one that needs longer. Check the network (proxy) and the engine, then retry. |
| `download_stalled` | A download (what a step needs, fetched in a phase of its own before it: a Compose start's images, a Vagrant start's boxes; or a step the manifest marks a download) showed no progress for as long as a download may, and was stopped. | The download stopped moving: check the network (proxy, VPN, the registry's or the box catalog's status) and the free disk space, then retry: the image layers it finished are kept. |
| `download_failed` | A download couldn't be made: its tool failed (the message says why: a registry or a box catalog refusing it, a name it doesn't know, the network). | Read the reason in the message: a registry refusing the image (its rate limit, a sign-in), a box or version the catalog doesn't have, a name it doesn't know, the network. Fix it, then retry. |
| `already_running` | The run is already up. | Use it as it is, `down` it first, or `up --replace`. |
| `running_elsewhere` | The run is recorded on another place (server or account) than the one asked for. | Act on the run with the `--host-label` it was started with. |
| `port_in_use` | A host port the run publishes is taken. | Stop what holds the port, or start the run with other published ports. |
| `not_parkable` | The manifest has no park steps for that mode. | Park it in the other mode (`--mode`), or `down` it. |
| `nothing_to_resume` | Nothing of the run, or not every one of its machines, is left to resume. | Start it again with `up`. |
| `not_running` | The run isn't up. | Start it with `up` (or `resume` a parked run) first. |
| `not_ready` | A ready condition didn't hold. | Read the `ready` events: the service the condition waits for didn't come up. `status`, then `down` and `up` again. |
| `secret_missing` | A required secret or input has no value. | Give the value: `--secret-from-env NAME[=VAR]`, with the variable set. |
| `param_invalid` | A param is unknown, missing or holds a value it doesn't allow. | Fix the `--param NAME=VALUE` the message names (a value the manifest allows). |
| `run_name` | A run name that can't name a run. | Give `--name` 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit. |
| `provider_refused` | The run's Vagrant provider is missing, unknown, not on this host, refused by the run, or lacks its plugin. | Pick a provider the run supports and this host has (`--provider`), with its Vagrant plugin installed. |
| `elevation_failed` | The elevation Hyper-V's commands need couldn't be had or was lost: the permission prompt declined or unanswered, the elevated session not started or ended before the operation (the message says which). | Approve the Windows permission prompt, or run as a member of Hyper-V Administrators; the message says what ended an elevated session. |
| `machine_unknown` | A machine the run doesn't have. | Name a machine of the manifest. |
| `machine_ambiguous` | The run has several machines the call could mean, and it named none. | Name the machine (`--machine`): the message lists the run's. |
| `network_unknown` | A network the run doesn't have. | Name a network of the manifest. |
| `slots_full` | Every slot of the place is held by another run. | `down` a run on the same place first. |
| `adopt_refused` | A deployment can't be taken over as a run. | The message says why the deployment can't be taken over; start the run with `up` instead. |
| `unreachable` | A machine a step goes to over SSH never answered. | Check the machine booted and its network (SSH, `--ssh-jump`), then retry. |
| `not_sealed` | The run's seal failed: it is up, but not sealed, so not ready. | `down` the run, which isn't ready, then `up` again. |
| `cancelled` | The operation was cut short before it ended (its future dropped: an embedder's cancel, the command line's Ctrl-C or SIGTERM); the tools it started were asked to stop, and are ended once they have, or 30 seconds later. | Nothing was left running by the tools; `status` shows what the run holds, and `up` clears what a cut start left. |
| `usage` | The embedder was asked for something that makes no sense (the command line: a run folder with no manifest, a value missing or malformed), so no operation started. | Fix the command line: `isothrum <command> --help`. |
| `no_console` | The machine has no graphical console to reach: its target or provider gives none (the error says why), it isn't running, or its hypervisor doesn't say where it is. | Read why in the message: start the machine, install what its hypervisor needs (VirtualBox's extension pack), or use its console (`isothrum console`) instead. |
