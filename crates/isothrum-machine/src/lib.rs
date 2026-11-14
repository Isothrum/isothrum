//! The machine-setup domain: which Vagrant providers (hypervisors) this computer can drive.
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller passes a `&dyn ProcessRunner`
//! (the real [`isothrum_runner::SystemProcesses`], or a fake in tests), and this crate holds no UI
//! types of its own.

mod exec;
pub mod providers;
