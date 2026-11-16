//! The machine check: what this computer has, in one report. The tools ([`crate::tools`]), the
//! Docker-compatible engines and their contexts ([`crate::docker`]), the package manager, the
//! cloud CLIs, the Vagrant providers ([`crate::providers`]), WSL 2 on Windows ([`crate::wsl`]) and
//! the groups this account joined since logging in. Each part is asked side by side; none fails
//! the check: a tool that doesn't answer reads as not installed.

use crate::context::Context;
use crate::docker::{self, Engine, EngineStatus, NoAnswer};
use crate::providers::{self, Provider, ProviderStatus};
use crate::tools::{self, CloudClis, PackageManager, Tool, probes};
use crate::wsl::{self, Marks, Wsl};

/// The Docker daemon the CLI uses, as the check found it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Daemon {
    /// Its operating system line (`docker info`), when it answered.
    pub answer: Result<String, NoAnswer>,
    /// The engine behind it, when it answered.
    pub engine: Option<Engine>,
}

impl Daemon {
    /// Whether it answers this account.
    #[must_use]
    pub fn running(&self) -> bool {
        self.answer.is_ok()
    }

    /// Whether it runs but refused this account.
    #[must_use]
    pub fn denied(&self) -> bool {
        self.answer.as_ref().err().is_some_and(NoAnswer::denied)
    }
}

/// What the machine check found on this computer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Check {
    /// The package manager a setup goes through.
    pub packages: PackageManager,
    /// The Docker CLI.
    pub docker: Tool,
    /// Docker Compose (`docker compose`).
    pub compose: Tool,
    /// The daemon the Docker CLI uses.
    pub daemon: Daemon,
    /// Every Docker-compatible engine found, each with its own state ([`docker::engines`]).
    pub engines: Vec<EngineStatus>,
    /// Vagrant.
    pub vagrant: Tool,
    /// Terraform.
    pub terraform: Tool,
    /// The cloud providers' CLIs.
    pub clouds: CloudClis,
    /// Every Vagrant provider offered here ([`providers::detect`]).
    pub providers: Vec<ProviderStatus>,
    /// The local hypervisors of `providers` ready for VMs ([`ready`]).
    pub ready: Vec<Provider>,
    /// WSL 2 (Windows only).
    pub wsl: Option<Wsl>,
    /// The groups this account joined since logging in (Linux), which apply from its next login.
    pub groups_pending: Vec<String>,
}

/// Checks the context's host: its tools, engines, providers, WSL (given the caller's own `marks`)
/// and the groups pending a login.
pub async fn check(cx: &Context, marks: Marks) -> Check {
    let (docker, compose, vagrant, terraform, daemon_os, clouds, packages) = tokio::join!(
        tools::version(cx, probes::DOCKER),
        tools::version(cx, probes::COMPOSE),
        tools::version(cx, probes::VAGRANT),
        tools::version(cx, probes::TERRAFORM),
        docker::daemon_os(cx),
        tools::cloud_clis(cx),
        tools::package_manager(cx),
    );
    let (providers, engines, wsl, groups_pending) = tokio::join!(
        providers::detect(cx, vagrant.installed),
        docker::engines(cx),
        wsl::check(cx, marks),
        providers::groups_pending(cx),
    );
    let engine = match &daemon_os {
        Ok(os) => Some(docker::engine_in_use(cx, os).await),
        Err(_) => None,
    };
    let ready = ready(&providers, vagrant.installed);
    Check {
        packages,
        docker,
        compose,
        daemon: Daemon { answer: daemon_os, engine },
        engines,
        vagrant,
        terraform,
        clouds,
        providers,
        ready,
        wsl,
        groups_pending,
    }
}

/// Of `statuses` ([`providers::detect`]'s), the local hypervisors ready for VMs: none without
/// Vagrant (`vagrant_installed`). A provider is ready when it is available, which holds only where
/// its hypervisor is installed.
#[must_use]
pub fn ready(statuses: &[ProviderStatus], vagrant_installed: bool) -> Vec<Provider> {
    if !vagrant_installed {
        return Vec::new();
    }
    statuses.iter().filter(|p| !p.remote && p.available).map(|p| p.provider).collect()
}

#[cfg(test)]
mod tests;
