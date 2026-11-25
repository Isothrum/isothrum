//! A tool context that says nothing and picks no port: a console's, a state folder's move.

use super::{PathBuf, Running, ToolCx, WarningCode};

/// What a tool context that says nothing and picks no port holds of its run: a console's, a
/// state folder's move.
pub(super) struct Quiet {
    key: String,
    state_dir: PathBuf,
    ssh_key: PathBuf,
}

impl Quiet {
    pub(super) fn of(run: &Running<'_>) -> Quiet {
        Quiet {
            key: run.runner.copy_key(&run.name),
            state_dir: run.state_dir(),
            ssh_key: crate::ssh::key_path(&run.runner.config.home),
        }
    }

    /// The context of `run` on these paths, its tools given `env`.
    pub(super) fn cx<'a>(&'a self, run: &'a Running<'_>, env: Vec<(String, String)>) -> ToolCx<'a> {
        ToolCx {
            project: &run.project,
            run: &run.name,
            key: &self.key,
            state_dir: &self.state_dir,
            provider: None,
            env,
            host_env: &run.runner.config.host_env,
            processes: run.runner.config.processes.as_ref(),
            ssh_key: &self.ssh_key,
            ports: &crate::formats::NoPorts,
            sink: &crate::formats::discard,
            warn: &unwarned,
            steps: crate::steps::Steps::silent(),
            made: crate::formats::NOTHING_MADE,
            fetched: None,
        }
    }
}

/// Says nothing of a warning: a quiet context's.
fn unwarned(_: WarningCode, _: &str) {}
