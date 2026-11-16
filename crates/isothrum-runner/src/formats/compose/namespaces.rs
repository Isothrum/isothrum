//! The services a resume stops before its start: those left in the network namespace of one that
//! stopped.

use super::READ_LIMIT;
use crate::cli::compose::{self, Container, Project};
use crate::formats::ToolCx;

/// The services that run (or restart) in the network namespace of one that doesn't run
/// (`network_mode: service:<host>`, the host stopped outside the run), directly or through
/// another such. Docker gives the host a new namespace when it starts again; they stay in the
/// old one, its interfaces gone, and `up --no-recreate` leaves them as they are and waits for
/// them: a sidecar that sets the host's routes ends there (or a restart of it fails), and the
/// resume with it. Stopped first, `up` starts them after their host, in its new namespace.
/// Files Compose can't read leave none stranded.
pub(super) async fn stranded(cx: &ToolCx<'_>, project: &Project<'_>, containers: &[Container]) -> Vec<String> {
    let hosts = match compose::namespace_hosts(&cx.read(READ_LIMIT), project).await {
        Ok(hosts) => hosts,
        Err(e) => {
            tracing::warn!(run = %cx.run, error = %e, "the project's network modes couldn't be read; no service is stopped before the start");
            return Vec::new();
        }
    };
    let runs = |service: &str| containers.iter().any(|c| c.service() == service && c.runs());
    let mut stranded: Vec<String> = Vec::new();
    loop {
        let more: Vec<String> = hosts
            .iter()
            .filter(|(service, host)| !stranded.contains(service) && (!runs(host) || stranded.contains(host)))
            .map(|(service, _)| service.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        stranded.extend(more);
    }
    stranded.retain(|s| containers.iter().any(|c| c.service() == s && c.runs_or_restarts()));
    stranded
}
