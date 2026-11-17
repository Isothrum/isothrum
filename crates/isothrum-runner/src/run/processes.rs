//! The tools of a run: an operation's recorded in the run's state folder (and elevated where its
//! hypervisor needs it), every `docker` call of the run sent to its Docker context, and what the
//! store keeps of what they did: the resources they made, what their downloads fetched.

use std::sync::Arc;

use super::Running;
use crate::Result;
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

    /// Records that the run's tool fetched `item` (an image through Docker context `context`,
    /// when the run names one). One the store can't record is said in the runner's log only: the
    /// operation goes on, and the download is never removed as the runner's.
    pub(super) fn record_fetched(&self, item: crate::DownloadItem, context: Option<&str>) {
        let item = match item {
            crate::DownloadItem::Image { reference, image_id, .. } => crate::DownloadItem::Image {
                reference,
                image_id,
                docker_context: context.map(str::to_owned),
            },
            other @ crate::DownloadItem::Box { .. } => other,
        };
        match self.runner.store.db().write(|tx| tx.record_download(Some(&self.name), &item)) {
            Ok(_) => tracing::info!(run = %self.name, ?item, "recorded what the run's download fetched"),
            Err(e) => tracing::warn!(run = %self.name, ?item, error = %e, "what the run's download fetched wasn't recorded"),
        }
    }

    /// The keys of the run's resources the store records as made by its tool: the provider's,
    /// with their id, not gone.
    pub(super) fn made(&self) -> Result<Vec<String>> {
        let rows = self.runner.store.db().read(|tx| tx.live_resources(&self.name))?;
        Ok(rows
            .into_iter()
            .filter(|r| r.scope == crate::store::Scope::Provider && r.native_id.is_some())
            .map(|r| r.key)
            .collect())
    }
}
