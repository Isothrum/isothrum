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
    /// It printed nothing for the call's stall limit ([`Call::ok_watched`]: a download that
    /// stopped moving), and was ended.
    #[error("`{tool}` printed nothing for {after_s}s and was stopped")]
    Stalled {
        /// The tool.
        tool: &'static str,
        /// The stall limit, in seconds.
        after_s: u64,
        /// The last line it printed, kept; none when it printed none.
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
            // Not on PATH: where its app installs it ([`installed`]: UTM, Parallels, VMware, VirtualBox).
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
        self.run_watched(tool, spec, None).await
    }

    /// [`Call::run_once`], ended as stalled once the tool printed nothing for `stall`: its
    /// progress is its lines.
    async fn run_watched<F: Display>(&self, tool: &'static str, spec: &ProcessSpec, stall: Option<Duration>) -> Result<Captured, CliError<F>> {
        let last: std::sync::Mutex<(Option<String>, tokio::time::Instant)> = std::sync::Mutex::new((None, tokio::time::Instant::now()));
        let kept = || last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).0.clone();
        let mut echo = |stream: Stream, line: &str| {
            if !line.trim().is_empty() {
                *last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (Some(crate::error::kept_line(line.trim_end())), tokio::time::Instant::now());
            }
            (self.sink)(stream, line);
        };
        let watch = async {
            let Some(stall) = stall else { return std::future::pending().await };
            loop {
                let due = last.lock().unwrap_or_else(std::sync::PoisonError::into_inner).1 + stall;
                if tokio::time::Instant::now() >= due {
                    return stall;
                }
                tokio::time::sleep_until(due).await;
            }
        };
        let ran = tokio::select! {
            ran = tokio::time::timeout(self.limit, capture_echoing(self.runner, spec, &mut echo)) => ran,
            after = watch => {
                return Err(CliError::Stalled {
                    tool,
                    after_s: after.as_secs(),
                    last_line: kept(),
                });
            }
        };
        match ran {
            Err(_) => Err(CliError::TimedOut {
                tool,
                limit_s: self.limit.as_secs(),
                last_line: kept(),
            }),
            Ok(Err(ProcessError::Missing { .. })) => Err(CliError::Missing { tool }),
            Ok(Err(e)) => Err(CliError::Spawn { tool, why: e.to_string() }),
            Ok(Ok(out)) => Ok(out),
        }
    }

    /// [`Call::ok`], the tool ended as [`CliError::Stalled`] once it printed nothing for `stall`
    /// (a download whose progress stopped). Not on `PATH`, it isn't looked for elsewhere: the
    /// tools watched so are on it.
    pub(crate) async fn ok_watched<F: Display>(
        &self,
        tool: &'static str,
        args: Vec<String>,
        stall: Duration,
        failure_of: impl FnOnce(&Captured) -> F,
    ) -> Result<Captured, CliError<F>> {
        let out = self.run_watched(tool, &self.spec(tool, args), Some(stall)).await?;
        if out.success() {
            Ok(out)
        } else {
            Err(CliError::Failed {
                tool,
                failure: failure_of(&out),
            })
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

/// The version `out` printed, read ([`crate::tool_output::version_found`]); `None` when it prints
/// none the runner reads (the tool is used, its minimum unchecked: the driver warns), `TooOld`
/// when it is older than `min`.
pub(crate) fn version<F: Display>(tool: &'static str, said: &str, min: Option<&str>) -> Result<Option<crate::project::platform::Version>, CliError<F>> {
    let Some(found) = crate::tool_output::version_found(said) else {
        tracing::warn!(tool, said = said.trim(), "no version the runner reads: the tool is used, its minimum unchecked");
        return Ok(None);
    };
    if let Some(min) = min.and_then(crate::project::platform::Version::parse)
        && found < min
    {
        return Err(CliError::TooOld {
            tool,
            found: found.to_string(),
            min: min.to_string(),
        });
    }
    Ok(Some(found))
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
        let fake = FakeProcesses::script()
            .expect("docker version", Reply::missing())
            .expect("vagrant status", Reply::fail(1, "boom"))
            .build();
        let call = testing::call(&fake);
        let missing = call.run::<String>("docker", argv(["version"])).await.unwrap_err();
        assert!(matches!(missing, CliError::Missing { tool: "docker" }), "{missing}");
        let failed = call.ok("vagrant", argv(["status"]), Captured::reason).await.unwrap_err();
        assert!(
            matches!(&failed, CliError::Failed { tool: "vagrant", failure } if failure == "boom (exited with code 1)"),
            "{failed}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_tool_past_its_limit_is_ended_as_timed_out_keeping_its_last_line() {
        let slow = Reply::ok("Waiting for machine to boot\nnever printed").paced(Duration::from_secs(40));
        let fake = FakeProcesses::script().expect("vagrant up", slow).build();
        let e = testing::call(&fake).run::<String>("vagrant", argv(["up"])).await.unwrap_err();
        assert!(
            matches!(&e, CliError::TimedOut { tool: "vagrant", limit_s: 60, last_line: Some(l) } if l == "Waiting for machine to boot"),
            "{e:?}"
        );
    }
}
