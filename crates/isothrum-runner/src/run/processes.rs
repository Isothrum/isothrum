//! The tools of a run: an operation's recorded in the run's state folder (and elevated where its
//! hypervisor needs it), and every `docker` call of the run sent to its Docker context.

use std::sync::Arc;

use super::Running;
use crate::options::RunOptions;
use crate::process::ProcessRunner;

impl Running<'_> {
    /// The process runner of an operation: its tools recorded in the run's state folder while they
    /// run, so a killed operation's are ended by the next ([`crate::process::ledger`]).
    pub(super) fn operation_processes(&self, opts: &RunOptions) -> Arc<dyn ProcessRunner> {
        let tools = self.state_dir().join(crate::process::ledger::DIR);
        let recorded: Arc<dyn ProcessRunner> = Arc::new(crate::process::Recording::new(
            Arc::clone(&self.runner.config.processes),
            tools,
            self.runner.operation_of(&self.name),
        ));
        let windows = self.runner.config.host == crate::project::platform::HostOs::Windows;
        if windows && self.driver.elevates(self.provider(opts)) {
            let env = self.runner.config.host_env.for_elevation();
            return Arc::new(crate::process::Elevated::new(recorded, self.state_dir().join("elevated"), env));
        }
        recorded
    }

    /// The Docker context the run's `docker` calls go to: the one `opts` names, else the one its
    /// start recorded (a `down` names none, and must reach the engine its start used).
    pub(super) fn docker_context(&self, opts: &RunOptions) -> Option<String> {
        opts.docker_context
            .clone()
            .or_else(|| self.runner.store.record(&self.name).ok().flatten()?.docker_context)
    }
}
