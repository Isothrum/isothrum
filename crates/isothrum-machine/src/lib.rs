//! The machine-setup domain: this computer ([`Host`]: its OS, CPU, package manager and the files
//! detection reads), and which Vagrant providers (hypervisors) it can drive.
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller hands it a [`Context`]: the runner (the
//! real [`isothrum_runner::SystemProcesses`], or a strict fake in tests), the [`Host`] and a probe
//! time limit. The crate keeps no statics and holds no UI types: a reason is a code
//! ([`providers::Unavailable`]) a UI translates, with an English `message()`.

mod context;
mod exec;
pub mod host;
pub mod providers;
#[cfg(test)]
mod testing;

pub use context::{Context, PROBE_TIMEOUT};
pub use host::{Arch, Devices, Host, LinuxPackages, Os, Packages};
