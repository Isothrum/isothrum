# Migrating to 2.0

Isothrum 2.0 removes public API that nothing used and changes some signatures, so it is a major
release. It also reads run manifest version 1, which the next Isoloom release writes, and refuses
the version 0 that 1.x read. A 1.x record (`record.json`) still loads. This page lists what to
change; [CHANGELOG.md](../CHANGELOG.md) has the detail of each item.

## Run manifest

| 1.x | 2.0 |
|---|---|
| Reads `manifest_version` 0 | Reads `manifest_version` 1 only; a version 0 `run.json` fails with `manifest_version` before anything runs. |

Regenerate the files with the Isoloom release that writes version 1 (`isoloom generate`). For a
run 1.x brought up, either take it down with 1.x before upgrading, or regenerate its files in the
same folder: the record names the `run.json` it was brought up from, and 2.0 reads the regenerated
one there. [Policies](policies.md#run-manifest-versions) has which Isothrum reads which version.

## Command line

| 1.x | 2.0 |
|---|---|
| `isothrum shell` | `isothrum console <machine>` prints the command that opens a console into any machine of the run (the access machine included). |
| `--ssh-jump` on any command | `--ssh-jump` on `up` only. It is recorded when the run starts; later operations go the same way. Elsewhere it did nothing. |
| `isothrum park` took no run options | `park` takes `--param`, `--secret-from-env`, `--host-label` and `--provider`, as `down` does. A run recorded under another `--host-label` is now refused (`running_elsewhere`). |
| `attach` on a run with no access machine stood up an ad-hoc box | It fails with `unsupported`. Declare an access machine in the spec, or add a machine with `update`. |
| Killed by SIGINT or SIGTERM | Exits 130 or 143 after stopping the tools it started; the operation ends with an `error` event, code `cancelled`. |
| A usage error with `--json` was text on standard error | It is an `error` event (code `usage`) on standard output. |
| `isothrum reap` passed when it couldn't query the cloud | It fails. |

New: `isothrum update`, `retire`, `console`, `completions`; `attach` and `detach` take `--param`
and `--secret-from-env`.

## Event stream

- An event's `run` and `op` are absent on an `error` written outside an operation (a usage error).
  Every event of an operation still has both.
- The warning code `hook_failed` is gone.
- New error codes: `cancelled`, `usage`, `machine_ambiguous`, `network_unknown`. New warning
  codes: `output_unformatted`, `record_not_saved`.
- Codes that were `unsupported` and now say what is wrong: an `attach` that names none of the
  run's several on-demand machines is `machine_ambiguous`; one whose `--network` the run doesn't
  have is `network_unknown`; a manifest value the runner can't use as written (a step's
  `verdict` that isn't a regular expression, an SSH login, an `ssh.run`'s `wait_s`, `host` or
  `command`, a `project.archive` without its folder, a `terraform.copy_module`'s `from` or
  `when`) is `manifest_invalid`.
- An `error` event for a timeout carries `timeout`: `{"kind": "no_progress", "after_s", "last_line"}`
  or `{"kind": "ceiling", "after_s"}`.
- A sensitive output is `***` in `status --json` too.

## `isothrum-runner`

| 1.x | 2.0 |
|---|---|
| `Running::park(mode, events)` | `Running::park(mode, opts, events)` |
| `Running::attach(...)`, `Running::detach(...)` | `attach(pick, opts, events)` with an `AccessPick` built from `AccessPick::default()` and its `with_*` methods; `detach(machine, opts, events)`. Pass `RunOptions::new()` for no options. |
| `Running::shell_command` | `Running::console_command(machine)` |
| `ShellCommand::cwd: Option<PathBuf>` | `ShellCommand::cwd: PathBuf` |
| `Error::Timeout { after_s, .. }` | `Error::Timeout { timed_out: TimedOut, .. }` (`NoProgress` or `Ceiling`; `timed_out.after_s()`) |
| `Error::PortInUse { protocol: &'static str }` | `protocol: String` |
| `ProcessError::Refused { arg }` | `ProcessError::Refused { why }` |
| `Event::run: String`, `Event::op: Op` | `Option<String>`, `Option<Op>` |
| `isothrum_runner::process::{Exit, ProcessError, OnLine, ...}` | At the crate root: `isothrum_runner::{Exit, ProcessError, OnLine, ProcessRunner, ProcessSpec, SystemProcesses}`. |
| `process::fake::{FakeProcesses, Reply, Call}` | `isothrum_runner::fake::{FakeProcesses, Reply, Call}` (feature `fake`); `Call` is `ProcessSpec`. |
| `SystemProcesses::new`, `SystemProcesses::for_host` | `SystemProcesses::default()`, which follows the Docker CLI's current context. |
| `RunnerConfig::with_processes`, `with_ports`, `with_clock` | Set `RunnerConfig::processes` and `RunnerConfig::ports` on `RunnerConfig::new(home)`; the clock is the system's. |
| `AccessPick`, `AttachReport` built with struct literals | Both are `#[non_exhaustive]`. |

Removed, with no replacement because nothing used them: the `Hooks` port (`Hooks`, `HookError`,
`RunView`, `Runner::add_hooks`); `RunRecord::manifest_sha256` and `LoadedManifest::sha256`;
`OutputValue::revealed`; `Runner::in_flight`; `process::capture` and `Captured`; `LoopbackPorts`
and `HeldPort::new` (`HostPorts` is sealed); `Running::outputs` (read `status().outputs`);
`Running::path`; `JsonLines` (an `Events` sink is a closure); `FixedClock`; the `Clock` port and
`SystemClock`; `this_host`; `Recorder`; `EVENTS_VERSION`; `FakePorts::take`, `FakePorts::answer`,
`Reply::after`, `FakeProcesses::lines` (read `FakeProcesses::calls`); `From<Option<i32>>` for
`Exit` (use `Exit::new`).

The run manifest's values Isoloom never wrote are no longer read: a `static` output, an expiry in
minutes from now, a `dotenv` state file and a WinRM guest delivery.

## `isothrum-machine`

The crate keeps only provider detection: `providers::{detect, id, Provider, ProviderStatus}`.

- Import `Provider` and `ProviderStatus` from `providers` (the crate root no longer re-exports
  them). `Provider` is Isoloom's `isoloom_manifest::Provider`; a provider's id is
  `providers::id(provider)` (was `Provider::id`).
- `Provider::Proxmox` and the cloud providers (`Aws`, `Azure`, `Gcp`, `DigitalOcean`, `Linode`,
  `Oci`, `Provider::is_cloud`) are gone: those are Terraform targets, not Vagrant providers.
- `ProviderStatus::available` is gone: read `reason.is_none()`. Its `remote`, `hypervisor`,
  `plugin`, `plugin_installed`, `hypervisor_version` and `plugin_version` are gone: `reason` says
  what is missing or too old.
- `Serialize` and `Deserialize` on `Provider` and `ProviderStatus` are gone.
- Gone with no user: the self-tests, the system report, `Dirs`, `Error`, `Result`, the `exec`
  module, the download record (`storage::*`), `Provider::ALL`, `Provider::is_remote` and the
  provider helpers.

## `isothrum-compiler`

`Compiled::yaml` and `Compiled::spec` are gone: `Compiled::into_parts()` returns the spec's text,
the spec and the project's files.

## `isothrum-wasm`

The JavaScript exports are unchanged, but `plan_json`, `own_files_json` and `choices_json` now
return `Result<String, JsError>` in Rust (JavaScript gets the same string, or an `Error`). The Rust
functions `plan`, `bundle_bytes`, `bundle_target_bytes`, `own_files` and `choices`, with `Error`
and `Result`, are internal; `Plan`, `File` and `Choices` stay.
