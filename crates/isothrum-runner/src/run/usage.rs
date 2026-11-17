//! What a run's machines use now (CPU and memory), as their engine reports it: one sample, read
//! from the tool that runs them, for a caller showing how much each deployment takes. A format
//! whose tool reports no such figure per machine gives none.

use super::Running;
use crate::Result;
use crate::events::Emitter;
use crate::formats::ToolCx;
use crate::options::RunOptions;

/// What one machine of a run uses now, as its engine reports it (several instances of it, a
/// service scaled up, summed).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct MachineUsage {
    /// Its name in the project.
    pub name: String,
    /// Its CPU use, in percent of one CPU (over 100 on several); none when the engine has no
    /// figure for it yet.
    pub cpu_percent: Option<f64>,
    /// The memory it uses, in bytes; none when the engine has no figure for it yet.
    pub memory_bytes: Option<u64>,
}

impl MachineUsage {
    /// Machine `name` using `cpu_percent` and `memory_bytes`.
    #[must_use]
    pub fn new(name: impl Into<String>, cpu_percent: Option<f64>, memory_bytes: Option<u64>) -> MachineUsage {
        MachineUsage {
            name: name.into(),
            cpu_percent,
            memory_bytes,
        }
    }
}

impl Running<'_> {
    /// What each running machine of the run uses now, by name, as its engine samples it once:
    /// a Compose project's containers (`docker stats`); none for a format whose tool reports no
    /// per-machine figure (Vagrant, Terraform). A machine not running has none. `opts` gives
    /// what the tools get. Emits nothing and never locks the run.
    ///
    /// # Errors
    ///
    /// As the tool fails: its engine doesn't answer, or its answer can't be read.
    pub async fn usage(&self, opts: &RunOptions) -> Result<Vec<MachineUsage>> {
        let emitter = Emitter::silent(self.name.as_str(), self.runner.config.clock.as_ref());
        self.with_cx(opts, &emitter, self.runner.config.processes.as_ref(), async |cx: &ToolCx<'_>| {
            self.driver.usage(cx).await
        })
        .await
    }
}
