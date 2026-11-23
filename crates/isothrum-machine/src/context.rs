//! The one handle machine setup works through: the process runner every tool runs on, the host
//! it reads, and how long a probe may take. The crate keeps no statics: a test hands its own
//! (a strict `FakeProcesses` script, a hand-built [`Host`]).

use std::sync::Arc;
use std::time::Duration;

use isothrum_runner::{Captured, ProcessRunner, ProcessSpec};

use crate::exec::{self, Unanswered};
use crate::host::Host;

/// How long a probe (`VBoxManage --version`, `vagrant plugin list`...) may take before it is
/// dropped: a tool that started and ran out of it reads as installed and slow (a cold `VBoxSVC`),
/// not as missing.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(25);

/// What machine setup runs its tools on and reads.
#[non_exhaustive]
pub struct Context {
    /// Every tool runs through it: `SystemProcesses`, or a fake in tests.
    pub runner: Arc<dyn ProcessRunner>,
    /// The computer it works on: [`Host::here`], or a hand-built one.
    pub host: Host,
    /// How long a probe may take ([`PROBE_TIMEOUT`] unless set).
    pub probe_timeout: Duration,
}

impl Context {
    /// Machine setup on `host`, its tools run by `runner`.
    #[must_use]
    pub fn new(runner: Arc<dyn ProcessRunner>, host: Host) -> Context {
        Context {
            runner,
            host,
            probe_timeout: PROBE_TIMEOUT,
        }
    }

    /// Machine setup on this computer ([`Host::here`]), its tools run by `runner`.
    pub async fn here(runner: Arc<dyn ProcessRunner>) -> Context {
        let host = Host::here(runner.as_ref()).await;
        Context::new(runner, host)
    }

    /// The same, a probe given `limit` to answer.
    #[must_use]
    pub fn with_probe_timeout(self, limit: Duration) -> Context {
        Context { probe_timeout: limit, ..self }
    }

    /// Runs `program args` as a probe ([`exec::probe`], within [`Context::probe_timeout`]).
    pub(crate) async fn probe(&self, program: &str, args: &[&str]) -> Result<Captured, Unanswered> {
        exec::probe(self.runner.as_ref(), self.probe_timeout, program, args).await
    }

    /// Runs `spec` as a probe given `limit` to answer ([`exec::probe_spec`]).
    pub(crate) async fn probe_within(&self, limit: Duration, spec: &ProcessSpec) -> Result<Captured, Unanswered> {
        exec::probe_spec(self.runner.as_ref(), limit, spec).await
    }

    /// The standard output of `program args` when it exits 0 ([`exec::read_ok`], within
    /// [`Context::probe_timeout`]), else why not.
    pub(crate) async fn read_ok(&self, program: &str, args: &[&str]) -> Result<String, String> {
        exec::read_ok(self.runner.as_ref(), self.probe_timeout, program, args).await
    }

    /// Whether `program` is installed: it is, as long as it could be started (a tool that prints
    /// usage and exits non-zero, like `vmrun`, still exists, and so does one that ran out of the
    /// probe's time).
    pub(crate) async fn tool_starts(&self, program: &str, args: &[&str]) -> bool {
        self.probe(program, args).await.as_ref().map_or_else(Unanswered::tool_is_there, |_| true)
    }
}

impl std::fmt::Debug for Context {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("host", &self.host)
            .field("probe_timeout", &self.probe_timeout)
            .finish_non_exhaustive()
    }
}
