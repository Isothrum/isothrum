//! The manifest's tools, probed before a start (Isoloom's host readiness, moved in): each one
//! this host and provider use runs its read-only probe, and must answer at its minimum version,
//! so a missing tool or a stopped engine fails before anything is created.

use std::num::NonZeroU32;
use std::sync::LazyLock;

use isoloom_manifest::{Argv, HostArgv, Program, Step, StepId, Template, Tool};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, capture_step};

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
    // Docker 26 and older exit 0 with an empty line when the daemon doesn't answer: an empty
    // answer from its probe is a daemon down, as an exit 1 is (the review of 2026-11-13).
    if tool.name.as_str() == "docker" && out.trim().is_empty() {
        return Err(Error::ToolFailed {
            step: STEP.to_owned(),
            reason: "`docker` doesn't answer: its probe printed nothing (its daemon isn't running)".to_owned(),
        });
    }
    let Some(min) = &tool.min_version else { return Ok(()) };
    new_enough(tool.name.as_str(), min, &out)
}

/// Whether `tool`, its probe having printed `said`, is `min` or later: refused when it says an
/// older version; passed, and traced with what it said, when no version can be read in it.
fn new_enough(tool: &str, min: &isoloom_manifest::Version, said: &str) -> Result<()> {
    match crate::version_found(said) {
        Some(found) if found < *min => Err(Error::ToolTooOld {
            tool: tool.to_owned(),
            found: found.as_str().to_owned(),
            required: min.as_str().to_owned(),
        }),
        Some(_) => Ok(()),
        None => {
            tracing::warn!(tool, min = min.as_str(), said, "the tool's version couldn't be read: it passes unchecked");
            Ok(())
        }
    }
}

/// The container engine, `docker`, answers, for a tool run from `image`: its server's version
/// printed. Docker 26 and older exit 0 with an empty line when the daemon doesn't answer, so
/// the answer is read, not the exit code alone (the review of 2026-11-13).
async fn engine(cx: &Cx<'_>, id: StepId, docker: Program, image: &str) -> Result<()> {
    let probe = HostArgv::Every(Argv::new(docker, ["info", "--format", "{{.ServerVersion}}"].map(Template::text).into()));
    let mut step = Step::argv(id.clone(), probe);
    step.timeout_s = NonZeroU32::new(PROBE_TIMEOUT_S);
    let version = capture_step(cx, STEP, &step).await.map_err(|e| {
        named(e, "docker", |reason| {
            format!("Docker, which runs `{image}` on this host, doesn't answer: {reason}")
        })
    })?;
    if version.trim().is_empty() {
        return Err(Error::ToolFailed {
            step: id.to_string(),
            reason: format!("Docker, which runs `{image}` on this host, doesn't answer: `docker info` printed no server version (its daemon isn't running)"),
        });
    }
    Ok(())
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

    /// Regression: a version that couldn't be read passed without a word.
    #[tokio::test]
    async fn a_version_that_cant_be_read_passes_traced_and_an_older_one_is_refused() {
        let min = isoloom_manifest::Version::parse("1.6.0").unwrap();
        let (passed, trace) = crate::testing::traced(async { new_enough("terraform", &min, "no version here") }).await;
        assert!(passed.is_ok());
        assert!(
            trace.contains("couldn't be read") && trace.contains("terraform") && trace.contains("no version here"),
            "{trace}"
        );
        let old = new_enough("terraform", &min, "Terraform v1.6.0-rc.1\non linux_amd64").unwrap_err();
        assert_eq!(old.code(), crate::ErrorCode::ToolTooOld, "a release candidate is older than its release");
        assert!(new_enough("terraform", &min, "Terraform v1.9.2").is_ok());
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
