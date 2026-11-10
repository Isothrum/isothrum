//! The manifest's tools, probed before a start (Isoloom's host readiness, moved in): each one
//! this host and provider use runs its read-only probe, and must answer at its minimum version,
//! so a missing tool or a stopped engine fails before anything is created.

use std::num::NonZeroU32;

use isoloom_manifest::{Argv, HostArgv, Program, Step, StepId, Template, Tool};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, capture_step};
use crate::tool_output::version_in;

/// The step id the probes report under.
const STEP: &str = "tools";

/// How long a probe may take.
const PROBE_TIMEOUT_S: u32 = 25;

/// Probes every tool of the manifest this run uses.
pub(crate) async fn probe(cx: &Cx<'_>) -> Result<()> {
    let tools: Vec<&Tool> = cx.run.manifest().tools.iter().filter(|t| used(cx, t)).collect();
    if tools.is_empty() {
        return Ok(());
    }
    cx.events
        .stepped(STEP, Some("Checking the tools…"), async {
            for tool in tools {
                probe_one(cx, tool).await?;
            }
            Ok(())
        })
        .await
}

/// Whether this host and provider use `tool`.
fn used(cx: &Cx<'_>, tool: &Tool) -> bool {
    cx.scope.admits(&tool.hosts, &tool.providers)
}

async fn probe_one(cx: &Cx<'_>, tool: &Tool) -> Result<()> {
    let id = StepId::parse(STEP).map_err(|e| Error::Unsupported { what: e.to_string() })?;
    if let Some(image) = tool.container.as_ref().filter(|_| tool.container_hosts.contains(&cx.run.runner.config.host)) {
        // The tool runs from its pinned image here: the engine that runs it must answer, and
        // the image is the version the manifest pins.
        return engine(cx, id, image.as_str()).await;
    }
    let mut step = Step::argv(id, tool.probe.clone());
    step.timeout_s = NonZeroU32::new(PROBE_TIMEOUT_S);
    let out = capture_step(cx, STEP, &step)
        .await
        .map_err(|e| named(e, tool.name.as_str(), |reason| format!("`{}` doesn't answer: {reason}", tool.name)))?;
    let Some(min) = &tool.min_version else { return Ok(()) };
    match (version_in(&out), version_in(min.as_str())) {
        (Some(found), Some(required)) if found < required => Err(Error::ToolTooOld {
            tool: tool.name.to_string(),
            found: format!("{}.{}.{}", found.0, found.1, found.2),
            required: min.as_str().to_owned(),
        }),
        _ => Ok(()),
    }
}

/// The container engine answers, for a tool run from `image`.
async fn engine(cx: &Cx<'_>, id: StepId, image: &str) -> Result<()> {
    let probe = HostArgv::Every(Argv::new(
        Program::parse("docker").map_err(|e| Error::Unsupported { what: e.to_string() })?,
        ["info", "--format", "{{.ServerVersion}}"].map(Template::text).into(),
    ));
    let mut step = Step::argv(id, probe);
    step.timeout_s = NonZeroU32::new(PROBE_TIMEOUT_S);
    capture_step(cx, STEP, &step).await.map(drop).map_err(|e| {
        named(e, "docker", |reason| {
            format!("Docker, which runs `{image}` on this host, doesn't answer: {reason}")
        })
    })
}

/// A probe's failure, said of the tool it probes: missing under its manifest name, failing with
/// `failed` saying why.
fn named(e: Error, tool: &str, failed: impl FnOnce(String) -> String) -> Error {
    match e {
        Error::ToolMissing { step: probe, .. } => Error::ToolMissing {
            step: probe,
            tool: tool.to_owned(),
        },
        Error::ToolFailed { step: probe, reason } => Error::ToolFailed {
            step: probe,
            reason: failed(reason),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probes_failure_is_said_of_its_tool() {
        let missing = named(
            Error::ToolMissing {
                step: "tools".into(),
                tool: "vagrant.exe".into(),
            },
            "vagrant",
            |r| r,
        );
        assert!(matches!(missing, Error::ToolMissing { step, tool } if step == "tools" && tool == "vagrant"));
        let failed = named(
            Error::ToolFailed {
                step: "tools".into(),
                reason: "exit 1".into(),
            },
            "x",
            |r| format!("x: {r}"),
        );
        assert!(matches!(failed, Error::ToolFailed { reason, .. } if reason == "x: exit 1"));
        let other = named(
            Error::Timeout {
                step: "tools".into(),
                timed_out: crate::TimedOut::NoProgress { after_s: 25, last_line: None },
            },
            "x",
            |r| r,
        );
        assert!(matches!(other, Error::Timeout { timed_out, .. } if timed_out.after_s() == 25));
    }
}
