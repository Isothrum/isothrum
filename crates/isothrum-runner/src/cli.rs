//! The adapters: one per external CLI, each owning the tool's invocation (its arguments, the C
//! locale for its messages via the process layer, its time limit) and turning what it prints
//! (text, JSON, exit codes, localized or versioned wordings) into typed values. A driver runs a
//! project's lifecycle by calling adapters; it never parses a tool's text nor walks untyped JSON,
//! and the shared core never names a tool.
//!
//! Every adapter fails with a [`CliError`]: the tool missing, too old, past its limit, printing
//! what can't be read, or failing with its own typed `Failure` (a refusal a driver acts on, with
//! `Other` last). Each wording an adapter knows is backed by a fixture of a named tool version
//! (`tool_output/fixtures/<tool>/<version>-<case>`); no test runs a live tool.

pub(crate) mod compose;
pub(crate) mod docker;
mod installed;
pub(crate) mod prlctl;
pub(crate) mod proxmox;
pub(crate) mod qemu;
pub(crate) mod terraform;
pub(crate) mod utmctl;
pub(crate) mod vagrant;
pub(crate) mod vboxmanage;
pub(crate) mod virsh;

use std::fmt::Display;
use std::path::PathBuf;
use std::time::Duration;

use crate::events::Stream;
use crate::process::{Captured, ProcessError, ProcessRunner, ProcessSpec, capture_echoing};

/// What a driver hands an adapter for one call: how to start processes, where, with which
/// variables (the run's values, its key, the operation), for how long, and where each line goes as
/// it comes (the operation's step output).
pub(crate) struct Call<'a> {
    /// Starts the tool.
    pub(crate) runner: &'a dyn ProcessRunner,
    /// The folder it runs in (the project's).
    pub(crate) cwd: Option<PathBuf>,
    /// Variables added to its environment.
    pub(crate) env: Vec<(String, String)>,
    /// How long it may run before it is ended.
    pub(crate) limit: Duration,
    /// Each line it prints, as it prints it.
    pub(crate) sink: &'a (dyn Fn(Stream, &str) + Send + Sync),
    /// Where an app installs a tool that isn't on `PATH` ([`installed`]).
    pub(crate) host_env: &'a crate::host_env::HostEnv,
}

/// Why an adapter's call didn't give its value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum CliError<F: Display> {
    /// The tool isn't installed or isn't on PATH.
    #[error("`{tool}` isn't installed or isn't on PATH")]
    Missing {
        /// The tool.
        tool: &'static str,
    },
    /// The tool is older than the adapter drives.
    #[error("`{tool}` is {found}, older than the {min} this runner drives")]
    TooOld {
        /// The tool.
        tool: &'static str,
        /// The version it says.
        found: String,
        /// The oldest version the adapter takes.
        min: String,
    },
    /// It ran past the call's limit, and was ended.
    #[error("`{tool}` was still running after {limit_s}s, and was stopped")]
    TimedOut {
        /// The tool.
        tool: &'static str,
        /// The call's limit, in seconds.
        limit_s: u64,
        /// The last line it printed, kept ([`crate::error::kept_line`]); none when it printed none.
        last_line: Option<String>,
    },
    /// It printed what the adapter can't read: never read as empty or absent.
    #[error("`{tool}` printed what can't be read: {what}")]
    Unexpected {
        /// The tool.
        tool: &'static str,
        /// What couldn't be read, and why.
        what: String,
    },
    /// It couldn't start for another reason than being missing.
    #[error("`{tool}` couldn't start: {why}")]
    Spawn {
        /// The tool.
        tool: &'static str,
        /// Why.
        why: String,
    },
    /// It ran and failed, as the adapter reads its failure.
    #[error("`{tool}` failed: {failure}")]
    Failed {
        /// The tool.
        tool: &'static str,
        /// Its failure.
        failure: F,
    },
}

impl Call<'_> {
    /// Runs `tool` with `args` to its end within the call's limit, each line to the sink: what it
    /// printed, whatever its exit (see [`Call::ok`]).
    pub(crate) async fn run<F: Display>(&self, tool: &'static str, args: Vec<String>) -> Result<Captured, CliError<F>> {
        self.run_spec(tool, self.spec(tool, args)).await
    }

    /// [`Call::run`], with the file at `input` piped into the tool's standard input (a request's
    /// credentials, which never go on its command line).
    pub(crate) async fn run_fed<F: Display>(&self, tool: &'static str, args: Vec<String>, input: &std::path::Path) -> Result<Captured, CliError<F>> {
        let mut spec = self.spec(tool, args);
        spec.stdin = Some(input.to_path_buf());
        self.run_spec(tool, spec).await
    }

    /// Runs `spec` (of `tool`) to its end within the call's limit, each line to the sink.
    async fn run_spec<F: Display>(&self, tool: &'static str, spec: ProcessSpec) -> Result<Captured, CliError<F>> {
        match self.run_once(tool, &spec).await {
            // Not on PATH: where its app installs it (UTM's app, VirtualBox's folder on Windows).
            Err(CliError::Missing { .. }) => match installed::find(tool, self.host_env) {
                installed::Installed::At(path) => {
                    let mut at = spec;
                    at.program = path.display().to_string();
                    self.run_once(tool, &at).await
                }
                installed::Installed::AppWithout(why) => Err(CliError::Spawn { tool, why }),
                installed::Installed::Nothing => Err(CliError::Missing { tool }),
            },
            other => other,
        }
    }

    /// Runs `spec` once, as [`Call::run_spec`] does.
    async fn run_once<F: Display>(&self, tool: &'static str, spec: &ProcessSpec) -> Result<Captured, CliError<F>> {
        let mut last: Option<String> = None;
        let mut echo = |stream: Stream, line: &str| {
            if !line.trim().is_empty() {
                last = Some(crate::error::kept_line(line.trim_end()));
            }
            (self.sink)(stream, line);
        };
        let ran = tokio::time::timeout(self.limit, capture_echoing(self.runner, spec, &mut echo)).await;
        match ran {
            Err(_) => Err(CliError::TimedOut {
                tool,
                limit_s: self.limit.as_secs(),
                last_line: last,
            }),
            Ok(Err(ProcessError::Missing { .. })) => Err(CliError::Missing { tool }),
            Ok(Err(e)) => Err(CliError::Spawn { tool, why: e.to_string() }),
            Ok(Ok(out)) => Ok(out),
        }
    }

    /// [`Call::run`], failing unless the tool exited 0: its failure read by `failure_of`.
    pub(crate) async fn ok<F: Display>(&self, tool: &'static str, args: Vec<String>, failure_of: impl FnOnce(&Captured) -> F) -> Result<Captured, CliError<F>> {
        let out = self.run(tool, args).await?;
        if out.success() {
            Ok(out)
        } else {
            Err(CliError::Failed {
                tool,
                failure: failure_of(&out),
            })
        }
    }
}

impl Call<'_> {
    /// The spec of `tool` with `args` in the call's folder with its variables, for the embedder to
    /// run (a console): nothing is started.
    pub(crate) fn spec(&self, tool: &str, args: Vec<String>) -> ProcessSpec {
        let mut spec = ProcessSpec::new(tool, args);
        spec.cwd.clone_from(&self.cwd);
        spec.env.clone_from(&self.env);
        spec
    }
}

/// The version `out` printed, read ([`crate::tool_output::version_found`]); `Unexpected` when it
/// prints none, `TooOld` when it is older than `min`.
pub(crate) fn version<F: Display>(tool: &'static str, said: &str, min: Option<&str>) -> Result<crate::project::platform::Version, CliError<F>> {
    let found = crate::tool_output::version_found(said).ok_or_else(|| CliError::Unexpected {
        tool,
        what: format!("no version in `{}`", said.trim()),
    })?;
    if let Some(min) = min.and_then(crate::project::platform::Version::parse)
        && found < min
    {
        return Err(CliError::TooOld {
            tool,
            found: found.to_string(),
            min: min.to_string(),
        });
    }
    Ok(found)
}

/// Whether any line `out` printed, in lower case, holds any of `wordings` (lower case).
pub(crate) fn said_any(out: &Captured, wordings: &[&str]) -> bool {
    out.stdout.iter().chain(&out.stderr_tail).any(|l| {
        let l = l.to_lowercase();
        wordings.iter().any(|w| l.contains(w))
    })
}

/// A removal `out` of `tool` that succeeded, or that failed saying (`gone`) the object was
/// gone already: either is done. Any other failure, with what the tool said.
pub(crate) fn done_or_gone(tool: &'static str, out: &Captured, gone: &[&str]) -> Result<(), CliError<String>> {
    if out.success() || said_any(out, gone) {
        return Ok(());
    }
    Err(CliError::Failed { tool, failure: out.reason() })
}

/// `args` as owned strings.
pub(crate) fn argv<const N: usize>(args: [&str; N]) -> Vec<String> {
    args.map(str::to_owned).to_vec()
}

#[cfg(test)]
pub(crate) mod testing {
    //! A [`Call`] over scripted processes, for the adapters' tests.

    use std::time::Duration;

    use super::Call;
    use crate::process::fake::FakeProcesses;

    /// Discards a line.
    pub(crate) fn quiet(_: crate::events::Stream, _: &str) {}

    /// A call through `fake`, in no folder, with a minute's limit.
    pub(crate) fn call(fake: &FakeProcesses) -> Call<'_> {
        Call {
            runner: fake,
            cwd: None,
            env: Vec::new(),
            limit: Duration::from_secs(60),
            sink: &quiet,
            host_env: &crate::host_env::EMPTY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::fake::{FakeProcesses, Reply};

    #[tokio::test]
    async fn a_missing_tool_and_a_failure_are_told_apart() {
        let fake = FakeProcesses::rules(&[("docker version", Reply::missing()), ("vagrant status", Reply::fail(1, "boom"))]);
        let call = testing::call(&fake);
        let missing = call.run::<String>("docker", argv(["version"])).await.unwrap_err();
        assert!(matches!(missing, CliError::Missing { tool: "docker" }), "{missing}");
        let failed = call.ok("vagrant", argv(["status"]), Captured::reason).await.unwrap_err();
        assert_eq!(failed.to_string(), "`vagrant` failed: boom (exited with code 1)");
    }
}
