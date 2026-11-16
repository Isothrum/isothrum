//! The Isothrum runner: runs a project folder (a Compose project, a Vagrantfile, a Terraform
//! module) through its own tool, with that tool's own lifecycle, and reads nothing else of it.
//!
//! The format comes from the folder's files ([`Format::detect`]), and each format has one driver
//! that knows its tool: Compose's `up --wait` and `down`, Vagrant's `up`, `suspend`, `halt` and
//! `destroy`, Terraform's `init`, `apply` and `destroy`. The runner adds what only it can know: the
//! run's name, key and private state, one operation per run at a time, journaled in the home's
//! store, the values and secrets the tools get in their environment, the resources each tool
//! reports, and the event stream an embedder (an app, the `isothrum` command line) shows. It
//! depends on no UI and on no generator: any project of a format it runs is a project.
//!
//! # Example
//!
//! Opening a folder detects its format, and nothing runs yet:
//!
//! ```
//! use isothrum_runner::{Format, RunName, Runner, RunnerConfig};
//!
//! let home = tempfile::tempdir()?;
//! let dir = tempfile::tempdir()?;
//! std::fs::write(dir.path().join("compose.yaml"), "services:\n  web:\n    image: nginx\n")?;
//!
//! let runner = Runner::new(RunnerConfig::new(home.path()))?;
//! let run = runner.open(dir.path(), Some(RunName::new("demo")?))?;
//! assert_eq!(run.project().format, Format::Compose);
//! assert_eq!(run.name().as_str(), "demo");
//! assert!(runner.runs()?.is_empty(), "no operation has run");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Running it takes its tool (here Docker), and an event sink ([`Events`], implemented for
//! closures):
//!
//! ```no_run
//! # async fn demo(run: &isothrum_runner::Running<'_>) -> isothrum_runner::Result<()> {
//! use isothrum_runner::{Event, Park, RunOptions};
//!
//! let events = |event: &Event| println!("{}", serde_json::to_string(event).unwrap_or_default());
//! let opts = RunOptions::new().with_env("GREETING", "hello");
//! let started = run.up(&opts, &events).await?;
//! let report = run.exec("web", &["true".to_owned()], &opts, &events).await?;
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
//! `FakeProcesses` does for the tools. A few programs run outside [`ProcessRunner`], each for a
//! reason of its own: on Windows `whoami` and `icacls`, which make the home owner-only when the
//! runner is built ([`Runner::new`]), before any operation, and, only for a tool a killed
//! operation left running, `ps` (Windows: PowerShell) to tell it is still that tool, and on
//! Windows `taskkill` to end it. Whether the process holding a lease of the home's store still runs
//! is the system's answer (Unix: signal 0; Windows: `OpenProcess`).
//!
//! # Inputs
//!
//! Values ([`RunOptions::env`]) and secrets ([`RunOptions::secrets`]) reach the tools only in
//! their environment, never on a command line and never in a file: Compose interpolates `${NAME}`
//! from it, a Vagrantfile reads `ENV`, and Terraform also gets each as `TF_VAR_<NAME>`. Every
//! tool of a run also gets the run's key ([`RUN_KEY_VAR`]). A secret is masked in every event.
//!
//! # Concurrency
//!
//! The runner is **async**, on Tokio, chosen once for the whole crate: every tool runs as an async
//! process with a time limit, and dropping an operation's future stops the process trees it
//! started: on Unix each gets SIGINT (so Terraform saves its state) and SIGKILL once it exited or
//! 30 seconds later; on Windows Ctrl-C on its own console, then its job ended the same way
//! ([`SystemProcesses`]). Operations on one run in one [`Runner`] wait for each other; an operation
//! on it from another process (or another runner) fails with `locked` once it has waited a second
//! for the run's lock. Operations on different runs run side by side. One exception to "no
//! blocking call in async code": the home's store ([`store::Store`], SQLite) and the run's lock are
//! a few KB on the local disk, each transaction a few rows, so they are read and written inline
//! rather than through `spawn_blocking`.
//!
//! # Errors and tracing
//!
//! Every fallible call returns [`Result`], whose [`Error`] has a stable [`ErrorCode`]; an
//! operation also reports its failure as its last event. The runner logs through `tracing` (spans
//! `run` and `driver`, events for each process), never by printing.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "the unit tests start real processes to stand in for a tool or to read what one did; each spawn of the product code says why"
    )
)]

mod adopt;
mod cli;
mod clock;
mod console;
mod encoding;
mod error;
mod events;
mod formats;
mod future;
mod graphical;
mod host_env;
mod name;
mod net;
mod network_env;
mod options;
mod port;
mod process;
mod project;
mod reap;
mod run;
mod runner;
mod secrets;
mod ssh;
mod state;
mod status;
pub mod store;
#[cfg(test)]
mod testing;
mod tool_output;

pub use adopt::Adoption;
pub use clock::{rfc3339, rfc3339_seconds};
pub use console::ShellCommand;
pub use encoding::{every_name, from_wire_name, percent_encoded, wire_name};
pub use error::{DETAIL_VERSION, Detail, DownloadKind, Error, ErrorCode, ErrorDetail, ErrorReport, PortHolder, ProviderNeed, Result, TimedOut};
pub use events::{Event, EventKind, EventLog, Events, OperationPhase, ResultData, ShownOutput, StepState, Stream, SweepFailure, WarningCode, events_schema};
pub use future::BoxFuture;
pub use graphical::{Console, ConsoleOffer, ConsoleProtocol, Endpoint, NativeApp, NativeConsole, NativeNeed, NativeOpen, ProxmoxVnc, Tunnel};
pub use name::RunName;
#[cfg(any(test, feature = "fake"))]
#[cfg_attr(docsrs, doc(cfg(feature = "fake")))]
pub use net::FakePorts;
pub use net::HostPorts;
pub use options::RunOptions;
pub use port::{GuestPort, HostPort};
#[cfg(any(test, feature = "fake"))]
#[cfg_attr(docsrs, doc(cfg(feature = "fake")))]
pub use process::fake;
pub use process::process_started_at;
pub use process::{Captured, Exit, HyperVRights, OnLine, ProcessError, ProcessFailed, ProcessRunner, ProcessSpec, SystemProcesses, capture, may_manage_hyperv};
pub use project::platform::{Arch, Cloud, HostOs, Protocol, Provider, Version};
pub use project::variables::{InputVariable, variables_of};
pub use project::{Format, Project};
pub use run::{ExecReport, Running, ServiceEndpoint};
pub use runner::{RUN_KEY_VAR, Runner, RunnerConfig, copy_key, this_arch, this_host};
pub use secrets::{SecretValue, Secrets};
pub use state::{Cause, Desired, IllegalTransition, Op, Park, RunState, Transition};
pub use status::{MachineState, MachineStatus, OutputValue, PublishedPort, Started, Status, status_schema};
pub use store::{LastOp, RunRecord};
pub use tool_output::{vagrant_plugins, version_found};
