//! The Isothrum runner: executes the run manifest Isoloom writes next to a target's generated
//! files (`.isoloom/<target>/run.json`), and reads nothing else of them.
//!
//! The manifest holds the native commands in order, the documented driver actions, readiness,
//! outputs, check runners, and the slots inputs and secrets travel through. The runner adds what
//! only it can know: the run's name and private state, one operation per run at a time, the
//! values of params and secrets, and the event stream an embedder (an app, the `isothrum` command
//! line) shows. It sits between Isoloom (whose
//! `isoloom-manifest` crate is the contract) and those embedders, and depends on no UI.
//!
//! # Example
//!
//! Opening a manifest reads and checks it, and nothing runs yet:
//!
//! ```
//! use isothrum_runner::{RunName, Runner, RunnerConfig};
//!
//! let home = tempfile::tempdir()?;
//! let dir = tempfile::tempdir()?;
//! let manifest = dir.path().join("run.json");
//! std::fs::write(&manifest, include_str!("../tests/fixtures/web-portal.run.json"))?;
//!
//! let runner = Runner::new(RunnerConfig::new(home.path()))?;
//! let run = runner.open(&manifest, Some(RunName::new("demo")?))?;
//! assert_eq!(run.manifest().environment.name.as_str(), "web-portal");
//! assert_eq!(run.name().as_str(), "demo");
//! assert!(runner.runs()?.is_empty(), "no operation has run");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Running it takes the tools it names (here Docker), and an event sink ([`Events`], implemented
//! for closures):
//!
//! ```no_run
//! # async fn demo(run: &isothrum_runner::Running<'_>) -> isothrum_runner::Result<()> {
//! use isothrum_runner::{Event, Park, RunOptions};
//!
//! let events = |event: &Event| println!("{}", serde_json::to_string(event).unwrap_or_default());
//! let opts = RunOptions::new().with_param("ISOLOOM_PUBLISH_FIXED", "1");
//! let started = run.up(&opts, &events).await?;
//! let report = run.check(&opts, &events).await?;
//! run.park(Park::Shutdown, &opts, &events).await?;
//! run.resume(&opts, &events).await?;
//! run.down(&opts, &events).await?;
//! # let _ = (started, report);
//! # Ok(())
//! # }
//! ```
//!
//! # Ports
//!
//! Every outside effect goes through a port: [`ProcessRunner`] (the tools), [`HostPorts`] (the
//! host's ports) and [`Events`] (where events go). [`ProcessRunner`] and [`Events`] are open traits,
//! since embedders implement them. [`HostPorts`] is sealed: [`RunnerConfig::new`] binds the
//! loopback itself, and the `fake` feature's `FakePorts` stands in for it in tests, as its
//! `FakeProcesses` does for the tools. Two programs run outside [`ProcessRunner`], each for a
//! reason of its own: Apple's `nc`, the macOS relays' byte pipe for as long as a connection lasts
//! (not a tool run to its end), and on Windows `whoami` and `icacls`, which make the home
//! owner-only when the runner is built ([`Runner::new`]), before any operation.
//!
//! # Deliveries
//!
//! Params and slot values reach the tools only through the manifest's deliveries, never on a
//! command line: `env` (a step's variables), `tf_var` (`TF_VAR_<var>`, a `map_entry` making one
//! JSON object), `state_file` (an owner-only Terraform variable file in the run's state folder,
//! written at `up`, kept for the destroy, removed by a successful `down`), and `guest` over SSH, Isoloom's delivery into a machine for a cloud with no secret
//! store (DigitalOcean, Linode): once the step it names (else every `up` step) succeeded, one
//! SSH session per machine, to the outputs `address_<machine>` (`-` as `_`) and
//! `ssh_user_<machine>` (else `ssh_user`), runs a fixed script as root that keeps each value
//! owner-only under `/var/lib/isoloom/secrets/<slot name>` and then touches
//! `/var/lib/isoloom/secrets/.delivered`, which the machine's set-up waits for; the values travel
//! base64-encoded on its standard input, retried while SSH can't connect (up to 15 minutes),
//! from an owner-only `guest-<machine>.in` in the run's state folder that is removed once
//! delivered (a removal that fails, fails the delivery).
//!
//! # Concurrency
//!
//! The runner is **async**, on Tokio, chosen once for the whole crate: every tool runs as an async
//! process with a time limit (a step's `provider_timeout_s` for the run's provider, else its
//! `timeout_s`, else an hour; 25 s for a probe; two minutes for an action's own Docker calls),
//! and dropping an operation's future ends the process tree it started. The limit counts the time
//! a tool shows no progress: it is stopped once it printed no new line for that long (a line, on
//! either stream, that isn't blank and isn't one of its last distinct lines; from its start when
//! it prints nothing), and at the latest six hours after its start, or at its limit when that is
//! longer ([`TimedOut`] says which). Operations on one run in one [`Runner`] wait for each other;
//! an operation on it from another process (or another runner) fails with `locked` once it has
//! waited a second for the run's lock. Operations on different runs run side by side. One
//! exception to "no blocking call in async code": the run's state files (a record, a lock, the
//! pinned ports, the event log) are a few KB on the local disk, so they are read and written
//! inline rather than through `spawn_blocking`.
//!
//! # Errors and tracing
//!
//! Every fallible call returns [`Result`], whose [`Error`] has a stable [`ErrorCode`]; an
//! operation also reports its failure as its last event. The runner logs through `tracing` (spans
//! `run`, `step` and `driver`, events for each process), never by printing.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod adopt;
mod archive;
mod checks;
mod clock;
mod console;
mod container;
mod driver;
mod encoding;
mod error;
mod events;
mod executor;
mod future;
#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;
mod guest;
mod host_env;
mod in_vm;
mod manifest;
mod name;
mod net;
mod network_env;
mod options;
mod outputs;
mod params;
mod process;
mod ready;
mod reap;
mod relay;
mod run;
mod runner;
mod scope;
mod secrets;
mod slot;
mod ssh;
mod state;
mod status;
mod store;
#[cfg(test)]
mod testing;
mod tools;
mod vars;

pub use adopt::Adoption;
pub use checks::{CheckReport, CheckResult};
pub use clock::rfc3339;
pub use console::ShellCommand;
pub use encoding::wire_name;
pub use error::{Error, ErrorCode, Result, TimedOut};
#[cfg(test)]
pub(crate) use events::Recorder;
pub use events::{Event, EventKind, EventLog, Events, ReadyState, StepState, Stream, WarningCode, events_schema};
pub use future::BoxFuture;
pub use isoloom_manifest;
pub use manifest::{LoadedManifest, load_manifest};
pub use name::RunName;
#[cfg(any(test, feature = "fake"))]
#[cfg_attr(docsrs, doc(cfg(feature = "fake")))]
pub use net::FakePorts;
pub use net::{HeldPort, HostPorts};
pub use options::{Occupied, RunOptions};
#[cfg(any(test, feature = "fake"))]
#[cfg_attr(docsrs, doc(cfg(feature = "fake")))]
pub use process::fake;
pub use process::{Exit, OnLine, ProcessError, ProcessRunner, ProcessSpec, SystemProcesses};
pub use reap::{Orphans, Place, Tagged};
pub use run::{AccessPick, AttachReport, Running};
pub use runner::{Runner, RunnerConfig};
pub use secrets::{SecretValue, Secrets};
pub use state::{Op, Park, RunState};
pub use status::{MachineState, MachineStatus, NetworkView, OutputValue, PublishedPort, Started, Status, status_schema};
pub use store::{LastOp, RunRecord};
