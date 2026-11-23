//! The machine-setup domain: this computer ([`Host`]: its OS, CPU, package manager and the files
//! detection reads), which Vagrant providers (hypervisors) it can drive, and the machine check
//! ([`check::check`]): its tools, Docker-compatible engines, package manager, cloud CLIs and, on
//! Windows, WSL 2; a guided setup's options and plan from that check ([`setup`]); installing and
//! removing the tools it runs projects with ([`install`]); the self-tests that prove it runs
//! deployments ([`selftest`]); what a run needs here and what of it is missing, with the plan that
//! brings it ([`requirements`]); and the cloud accounts its cloud CLIs
//! are signed in to ([`cloud`]): whether a sign-in holds, and what an account offers; the boxes
//! Vagrant holds here and adds ([`boxes`]); and this computer's public address, as independent
//! services agree on it ([`net`]).
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller hands it a [`Context`]: the runner (the
//! real [`isothrum_runner::SystemProcesses`], or a strict fake in tests), the [`Host`] and a probe
//! time limit. The crate keeps no statics and holds no UI types: a reason is a code
//! ([`providers::Unavailable`]) a UI translates, with an English `message()`.

pub mod boxes;
pub mod check;
pub mod cloud;
mod context;
pub mod docker;
mod exec;
pub mod failure;
pub mod host;
pub mod install;
pub mod net;
pub mod place;
pub mod providers;
pub mod requirements;
pub mod selftest;
pub mod setup;
pub mod terminal;
#[cfg(test)]
mod testing;
pub mod tools;
pub mod wsl;

pub use context::{Context, PROBE_TIMEOUT};
pub use failure::{Failure, Refusal};
pub use host::{Arch, Devices, Host, HostOs, LinuxPackages, Packages};
