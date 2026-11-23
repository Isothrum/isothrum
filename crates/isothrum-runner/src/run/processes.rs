//! The tools of a run: an operation's recorded in the run's state folder (and elevated where its
//! hypervisor needs it), every `docker` call of the run sent to its Docker context, and what the
//! store keeps of what they did: the resources they made, what their downloads fetched.

use std::sync::Arc;

use super::Running;
use crate::Result;
use crate::options::RunOptions;
use crate::process::ProcessRunner;

impl<'r> Running<'r> {
    /// The same run, every operation of this handle sharing one elevation where its hypervisor
    /// needs one (Hyper-V on Windows): one administrator prompt for an `up` and the `exec` after
    /// it, say, instead of one per operation. The elevated session ends when the handle drops.
    #[must_use]
    pub fn sharing_elevation(self) -> Running<'r> {
        Running {
            elevation: Some(std::sync::OnceLock::new()),
            ..self
        }
    }

    /// The process runner of an operation: its tools recorded in the run's state folder while they
    /// run, so a killed operation's are ended by the next ([`crate::process::ledger`]); elevated
    /// where its hypervisor needs it, in a session of its own or in the one every operation of a
    /// handle [sharing its elevation](Running::sharing_elevation) shares.
    pub(super) fn operation_processes(&self, opts: &RunOptions) -> Arc<dyn ProcessRunner> {
        let tools = self.state_dir().join(crate::process::ledger::DIR);
        let operation = self.runner.operation_of(&self.name);
        let base = Arc::clone(&self.runner.config.processes);
        let windows = self.runner.config.host == crate::project::platform::HostOs::Windows;
        if !(windows && self.driver.elevates(self.provider(opts))) {
            return Arc::new(crate::process::Recording::new(base, tools, operation));
        }
        let elevated = |inner: Arc<dyn ProcessRunner>| -> Arc<dyn ProcessRunner> {
            let env = self.runner.config.host_env.for_elevation();
            Arc::new(crate::process::Elevated::new(inner, self.state_dir().join("elevated"), env))
        };
        match &self.elevation {
            // The shared session under each operation's recording, which only adds its variables.
            Some(shared) => Arc::new(crate::process::Recording::new(
                Arc::clone(shared.get_or_init(|| elevated(base))),
                tools,
                operation,
            )),
            None => elevated(Arc::new(crate::process::Recording::new(base, tools, operation))),
        }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::process::ProcessSpec;
    use crate::process::fake::{FakeProcesses, Reply};
    use crate::project::platform::{HostOs, Provider};
    use crate::testing::{no_ports, processes, runner_config};
    use crate::{RunName, RunOptions, Runner};

    /// How many times an account's rights to manage Hyper-V were asked, after two operations'
    /// `vagrant status` on a Hyper-V run of a Windows runner, the handle sharing its elevation or
    /// not: each elevated session asks once.
    async fn sessions(share: bool) -> usize {
        let fake = Arc::new(
            FakeProcesses::script()
                .allow(
                    "whoami /groups",
                    Reply::ok("\"BUILTIN\\Hyper-V Administrators\",\"Alias\",\"S-1-5-32-578\",\"Enabled group\""),
                )
                .allow("vagrant status", Reply::ok(""))
                .build(),
        );
        let root = tempfile::tempdir().expect("a folder");
        let project = root.path().join("stack");
        std::fs::create_dir(&project).expect("the project");
        std::fs::write(project.join("Vagrantfile"), "Vagrant.configure(\"2\") do |config| end\n").expect("its file");
        let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), HostOs::Windows);
        let runner = Runner::new(config).expect("a runner");
        let run = runner.open(&project, Some(RunName::new("vm").expect("a name"))).expect("opened");
        let run = if share { run.sharing_elevation() } else { run };
        let opts = RunOptions::new().with_provider(Provider::Hyperv);
        for _ in 0..2 {
            let ran = run
                .operation_processes(&opts)
                .run(&ProcessSpec::new("vagrant", ["status"]), &mut |_, _| {})
                .await;
            assert!(ran.expect("ran").success());
        }
        fake.lines().iter().filter(|l| l.starts_with("whoami")).count()
    }

    #[tokio::test]
    async fn a_handle_sharing_its_elevation_opens_one_session_for_all_its_operations() {
        assert_eq!(sessions(false).await, 2, "one session per operation");
        assert_eq!(sessions(true).await, 1, "one for the handle");
    }
}
