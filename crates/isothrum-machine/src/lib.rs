//! The machine-setup domain: what this computer can run (Docker, Vagrant, the hypervisors) and the
//! self-tests that prove it by booting a throwaway container or VM. Shared by the Isothrum desktop
//! app and the Cyber CTF launcher.
//!
//! Every tool runs through the Isothrum runner's no-unsafe [`isothrum_runner::ProcessRunner`], so
//! nothing here forks a shell or touches raw process APIs. A caller passes a `&dyn ProcessRunner`
//! (the real [`isothrum_runner::SystemProcesses`], or a fake in tests), keeping this crate free of
//! the desktop app's Tauri types.

pub mod dirs;
pub mod error;
pub mod exec;
pub mod providers;
pub mod system;

pub use dirs::Dirs;
pub use error::{Error, Result};
pub use providers::{Provider, ProviderStatus};
pub use system::SystemReport;
