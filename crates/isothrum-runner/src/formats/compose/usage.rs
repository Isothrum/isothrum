//! What a run's running services use now: each container's figures from the engine's `stats`
//! (one sample of every running container, so one gone since `ps` listed it fails nothing), a
//! service's containers summed.

use std::collections::BTreeMap;

use super::READ_LIMIT;
use crate::Result;
use crate::cli::compose::Container;
use crate::cli::docker::{self, ContainerStats};
use crate::formats::{ToolCx, tool_error};

/// What each service of the `running` containers uses now; none when none runs.
pub(super) async fn of(cx: &ToolCx<'_>, running: &[Container]) -> Result<Vec<crate::MachineUsage>> {
    if running.is_empty() {
        return Ok(Vec::new());
    }
    let stats = docker::stats(&cx.read(READ_LIMIT)).await.map_err(|e| tool_error("usage", e))?;
    Ok(by_service(running, &stats))
}

/// What each service's running `containers` use, from the engine's `stats`: a service's
/// containers summed, a figure none when the engine has it for none of them.
fn by_service(containers: &[Container], stats: &[ContainerStats]) -> Vec<crate::MachineUsage> {
    let mut by_service: BTreeMap<&str, (Option<f64>, Option<u64>)> = BTreeMap::new();
    for c in containers {
        let seen = stats.iter().find(|s| s.is(c.id()));
        let (cpu, memory) = by_service.entry(c.service()).or_default();
        if let Some(p) = seen.and_then(|s| s.cpu_percent) {
            *cpu = Some(cpu.unwrap_or(0.0) + p);
        }
        if let Some(b) = seen.and_then(|s| s.memory_bytes) {
            *memory = Some(memory.unwrap_or(0).saturating_add(b));
        }
    }
    by_service
        .into_iter()
        .map(|(name, (cpu, memory))| crate::MachineUsage::new(name, cpu, memory))
        .collect()
}
