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
| `project_invalid` | The folder isn't a project Isothrum runs: it holds no Compose file, Vagrantfile or Terraform module, or the files of several formats with none named. | Point the command at the folder of one Compose project, Vagrantfile or Terraform module, or keep one format's files in it when it holds several. |
| `unsupported` | The manifest needs something this runner doesn't do (a target that doesn't run on this host, a delivery its driver doesn't take, an action of another driver's, a step with no command for this host), or the call asks the run for something its target can't do (a console into a Compose service with no VM, a Vagrant VM joined to another network). | Upgrade Isothrum, or ask the run for what its format does (the message says what it doesn't). |
| `io` | A file or folder couldn't be read or written, or a socket on this computer bound. | Check the path in the message: free space, permissions, the runner's home (`--home`, `ISOTHRUM_HOME`); for a port, the open-file limit (`ulimit -n`) and the loopback. |
| `locked` | Another process runs an operation on this run, or holds the home's slot pick. | Wait for the other operation on the run to end, or stop the process running it. |
| `tool_missing` | A tool a step runs isn't installed. | Install the tool, or put it on `PATH`. |
| `tool_failed` | A step's tool ran and failed. | Read the step's last lines (its `log` events, `isothrum events`): fix what the tool says, then run the command again. |
| `tool_unanswered` | A tool the run needs is installed, but got no answer from its service: Docker's engine stopped, or quit while a step ran (the `step` says which): the message says what the tool said. | Start the tool's service (Docker Desktop, the Docker engine), then retry. |
| `tool_too_old` | A tool answers at a version older than the run needs. | Upgrade the tool to the version the message names. |
| `tool_denied` | A tool refused this account (Docker's engine its socket or named pipe): the account needs the access (a group), not the tool started. | Give this account the tool's access (Linux: the `docker` group, then sign in again; Windows: the `docker-users` group), then retry. |
| `credentials_expired` | A cloud refused the sign-in the run's tools use: a CLI or SSO session that ran out, an expired token, application-default credentials gone stale, a profile never signed in. Signing in again fixes it; the cloud refused before Terraform changed anything. | Sign in to the cloud again with its CLI (`aws login`, `az login`, `gcloud auth application-default login`, `oci session authenticate`), then retry. |
| `address_pool_overlap` | A network the run makes needs an address block another network on the engine already holds (Docker's `Pool overlaps with other one on this address space`). | Another network holds the addresses the run needs: remove the network left behind (`docker network prune` removes the unused ones) or stop the run that holds it, then retry. |
| `timeout` | A step showed no progress for its time limit, or ran to the ceiling every step stops at, and was stopped, with what it started (the `error` event's `detail` says which). | See the event's `detail`: a step stuck with no output, or one that needs longer. Check the network (proxy) and the engine, then retry. |
| `download_stalled` | A download (what a step needs, fetched in a phase of its own before it: a Compose start's images, a Vagrant start's boxes; or a step the manifest marks a download) showed no progress for as long as a download may, and was stopped. | The download stopped moving: check the network (proxy, VPN, the registry's or the box catalog's status) and the free disk space, then retry: the image layers it finished are kept. |
| `download_failed` | A download couldn't be made: its tool failed (the message says why: a registry or a box catalog refusing it, a name it doesn't know, the network). A failure for the network that passes (a connection reset, refused or timed out, a gateway's 502, 503 or 504) was tried again first, up to three more times: the message is the last try's. | Read the reason in the message: a registry refusing the image (its rate limit, a sign-in), a box or version the catalog doesn't have, a name it doesn't know, the network. Fix it, then retry. |
| `already_running` | The run is already up. | Use it as it is, or `down` it first. |
| `running_elsewhere` | The run is recorded on another place (server or account) than the one asked for. | Act on the run with the `--host-label` it was started with. |
| `port_in_use` | A host port the run publishes is taken. | Stop what holds the port, or start the run with other published ports. |
| `not_parkable` | The manifest has no park steps for that mode. | Park it in the other mode (`--mode`), or `down` it. |
| `nothing_to_resume` | Nothing of the run, or not every one of its machines, is left to resume. | Start it again with `up`. |
| `not_running` | The run isn't up. | Start it with `up` (or `resume` a parked run) first. |
| `not_ready` | A ready condition didn't hold. | The project didn't say it was ready: `status`, then `down` and `up` again. |
| `secret_missing` | A required secret or input has no value. | Give the value: `--secret-from-env NAME[=VAR]`, with the variable set. |
| `input_invalid` | A value or secret that can't be handed to the tools (its name empty or holding `=`, a NUL in it). | Fix the `--env NAME=VALUE` or `--secret-from-env NAME` the message names: no `=` in a name, no NUL anywhere. |
| `run_name` | A run name that can't name a run. | Give `--name` 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit, not a Windows device name (`con`, `nul`, `com1`…). |
| `provider_refused` | The run's Vagrant provider is missing, unknown, not on this host, refused by the run, or lacks its plugin, or the hypervisor lacks what it needs on this computer (virtiofsd, for a shared folder on Linux: the `detail`'s `need`). | Pick a provider the run supports and this host has (`--provider`), with its Vagrant plugin installed; install what the `detail`'s `need` names (`virtiofsd`); allow the program driving the hypervisor's app to automate it in the system's settings (`automation`); quit the hypervisor's app and open it once from the system's file manager, or reinstall it (`app_approval`: the system never approved the app, and refused to run its helper). |
| `elevation_failed` | The elevation Hyper-V's commands need couldn't be had or was lost: the permission prompt declined or unanswered, the elevated session not started or ended before the operation (the message says which). | Approve the Windows permission prompt, or run as a member of Hyper-V Administrators; the message says what ended an elevated session. |
| `machine_unknown` | A machine the run doesn't have. | Name a machine of the project (`isothrum status` lists them). |
| `machine_ambiguous` | The run has several machines the call could mean, and it named none. | Name the machine (`--machine`): the message lists the run's. |
| `network_unknown` | A network the run doesn't have. | Name a network of the project. |
| `adopt_refused` | A deployment can't be taken over as a run. | The message says why the deployment can't be taken over; start the run with `up` instead. |
| `cancelled` | The operation was cut short before it ended (its future dropped: an embedder's cancel, the command line's Ctrl-C or SIGTERM); the tools it started were asked to stop, and are ended once they have, or 30 seconds later. | Nothing was left running by the tools; `status` shows what the run holds, and `up` clears what a cut start left. |
| `usage` | The embedder was asked for something that makes no sense (the command line: a run folder with no manifest, a value missing or malformed), so no operation started. | Fix the command line: `isothrum <command> --help`. |
| `no_console` | The machine has no graphical console to reach: its target or provider gives none (the error says why), it isn't running, or its hypervisor doesn't say where it is. | Read why in the message: start the machine, install what its hypervisor needs (VirtualBox's extension pack), or use its console (`isothrum console`) instead. |
| `server_auth_failed` | The server the run is on (Proxmox VE, ESXi) refused the run's credentials: its API answered 401 or 403, or its tool said the sign-in failed (an API token revoked, a password changed). | The server took the run's credentials no more: give the run its new API token or password (`--secret-from-env`), then retry. |
| `corrupt` | A file of the runner's own (a record, a ledger entry) is there but isn't what it wrote: not JSON, or another version's shape. | The file the message names is the runner's own and isn't what it wrote: another version's, or damaged. Remove it (a record: the run's folder). |
| `illegal_transition` | The run's state machine has no such move (a park of a parked run, a resume of a ready one), or a resource row can't move that way. | The run isn't where the operation starts from (a park of a parked run, a resume of a ready one): `status`, then the operation its state takes. |
| `state_conflict` | The run moved between the read and the write that would have moved it: try again. | Another operation moved the run meanwhile: `status`, then run the command again. |
| `store_version` | The home's store was written by a newer runner. | A newer Isothrum wrote this home's `state.db`: use that release, or another `--home`. |
| `store_corrupt` | The home's store isn't a database this runner reads, or holds what it doesn't write. | The home's `state.db` isn't what the runner wrote: restore it from a backup, or move it away (the runs it held are then forgotten; `reap` finds what a cloud still holds). |
| `lease_held` | A live process holds the lease asked for. | Another process holds the run: wait for it to end, or stop it. |
