//! The machine-setup domain: which Vagrant providers (hypervisors) this computer can drive, and
//! the record format of what a program downloaded for its own operations (container images,
//! Vagrant boxes), so it can later remove exactly those.
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller passes a `&dyn ProcessRunner`
//! (the real [`isothrum_runner::SystemProcesses`], or a fake in tests), and this crate holds no UI
//! types of its own.

mod exec;
pub mod providers;
pub mod storage;

pub use providers::{Provider, ProviderStatus};
