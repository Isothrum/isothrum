//! The machine-setup domain: this computer ([`Host`]: its OS, CPU, package manager and the files
//! detection reads), which Vagrant providers (hypervisors) it can drive, and the machine check
//! ([`check::check`]): its tools, Docker-compatible engines, package manager, cloud CLIs and, on
//! Windows, WSL 2; installing and removing the tools it runs projects with ([`install`]); and the
//! self-tests that prove it runs deployments ([`selftest`]).
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller hands it a [`Context`]: the runner (the
//! real [`isothrum_runner::SystemProcesses`], or a strict fake in tests), the [`Host`] and a probe
//! time limit. The crate keeps no statics and holds no UI types: a reason is a code
//! ([`providers::Unavailable`]) a UI translates, with an English `message()`.

pub mod check;
mod context;
pub mod docker;
mod exec;
pub mod host;
pub mod install;
pub mod providers;
pub mod selftest;
#[cfg(test)]
mod testing;
pub mod tools;
pub mod wsl;

pub use context::{Context, PROBE_TIMEOUT};
pub use host::{Arch, Devices, Host, HostOs, LinuxPackages, Packages};
