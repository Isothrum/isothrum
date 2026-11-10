//! The manifest's tools, probed before a start (Isoloom's host readiness, moved in): each one
//! this host and provider use runs its read-only probe, and must answer at its minimum version,
//! so a missing tool or a stopped engine fails before anything is created.

use std::num::NonZeroU32;
use std::sync::LazyLock;

use isoloom_manifest::{Argv, HostArgv, Program, Step, StepId, Template, Tool, Version};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, capture_step};
use crate::tool_output::version_in;

/// The step id the probes report under.
const STEP: &str = "tools";

/// [`STEP`] and the container engine's program, as Isoloom's types, parsed once for the process:
/// literals a test holds to Isoloom's rules, so the error each keeps can't happen.
static CONSTANTS: LazyLock<std::result::Result<(StepId, Program), String>> = LazyLock::new(|| {
    Ok((
        StepId::parse(STEP).map_err(|e| e.to_string())?,
        Program::parse("docker").map_err(|e| e.to_string())?,
    ))
});

/// The probes' step id and the container engine's program ([`CONSTANTS`]).
fn constants() -> Result<(StepId, Program)> {
    CONSTANTS.clone().map_err(|what| Error::Unsupported { what })
}

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
    let (id, docker) = constants()?;
    if let Some(image) = tool.container.as_ref().filter(|_| tool.container_hosts.contains(&cx.scope.host)) {
        // The tool runs from its pinned image here: the engine that runs it must answer, and
        // the image is the version the manifest pins.
        return engine(cx, id, docker, image.as_str()).await;
    }
    let mut step = Step::argv(id, tool.probe.clone());
    step.timeout_s = NonZeroU32::new(PROBE_TIMEOUT_S);
    let out = capture_step(cx, STEP, &step)
        .await
        .map_err(|e| named(e, tool.name.as_str(), |reason| format!("`{}` doesn't answer: {reason}", tool.name)))?;
    let Some(min) = &tool.min_version else { return Ok(()) };
    match found_version(&out) {
        Some(found) if found < *min => Err(Error::ToolTooOld {
            tool: tool.name.to_string(),
            found: found.as_str().to_owned(),
            required: min.as_str().to_owned(),
        }),
        _ => Ok(()),
    }
}

/// The version a probe's output `out` says, as Isoloom's [`Version`], to compare with the
/// manifest's minimum in Isoloom's order (a pre-release before its release): its first word that
/// is one (`v2.29.1`, `1.0.0-rc.1`), else its first `<major>.<minor>[.<patch>]` (`2.3.8.dev` is
/// 2.3.8, a missing patch 0). None when it says none: nothing is held back then.
fn found_version(out: &str) -> Option<Version> {
    out.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        .map(|w| w.strip_prefix('v').unwrap_or(w))
        .find_map(|w| Version::parse(w).ok())
        .or_else(|| {
            let (major, minor, patch) = version_in(out)?;
            Version::parse(&format!("{major}.{minor}.{patch}")).ok()
        })
}

/// The container engine, `docker`, answers, for a tool run from `image`.
async fn engine(cx: &Cx<'_>, id: StepId, docker: Program, image: &str) -> Result<()> {
    let probe = HostArgv::Every(Argv::new(docker, ["info", "--format", "{{.ServerVersion}}"].map(Template::text).into()));
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
    fn the_probes_constants_are_ones_isoloom_takes() {
        let (id, docker) = constants().unwrap();
        assert_eq!((id.as_str(), docker.as_str()), (STEP, "docker"));
    }

    #[test]
    fn a_tools_version_is_read_and_ordered_as_isoloom_orders_versions() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(found_version("Docker Compose version v2.29.1"), Some(v("2.29.1")));
        assert_eq!(found_version("Vagrant 2.3.8.dev"), Some(v("2.3.8")));
        assert_eq!(found_version("2.4"), Some(v("2.4.0")));
        assert_eq!(found_version("no version here"), None);
        // Regression: the probe compared numbers only, so a release candidate passed for the
        // release it leads to.
        let rc = found_version("Terraform v1.6.0-rc.1\non linux_amd64").unwrap();
        assert_eq!(rc, v("1.6.0-rc.1"));
        assert!(rc < v("1.6.0"), "a pre-release is older than its release");
    }

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
