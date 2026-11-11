//! One argv step: the command filled in for this host and run, its lines streamed as `log`
//! events (masked), stopped with everything it started once it shows no progress for its time
//! limit ([`super::progress`]). A download (a step the manifest marks `download`) has no ceiling,
//! and its stall and failure are `download_stalled` and `download_failed`.

use std::collections::VecDeque;
use std::time::Duration;

use isoloom_manifest::{Action, Run, Step};
use regex::Regex;

use super::delivery::step_env;
use super::progress::{Progress, downloading, within};
use super::{Cx, plan};
use crate::Result;
use crate::error::Error;
use crate::events::Stream;
use crate::process::{Captured, ProcessError, ProcessSpec, STDOUT_REASON_LINES, Tail, capture_echoing};
use crate::scope::Scope;

/// Runs argv step `step` of list `list`; `observe` sees each line too (a check runner's results).
/// An `ssh.run` with a `wait_s` is tried again while its host doesn't answer
/// ([`super::reconnect`]); any other step runs as its driver runs a command
/// ([`AnyDriver::run_argv`](crate::driver::AnyDriver::run_argv)).
pub(crate) async fn run_argv(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
    if let Some(wait) = crate::ssh::wait_of(cx, list, step)? {
        return super::reconnect::run(cx, list, step, observe, &wait).await;
    }
    cx.run.driver.run_argv(cx, list, step, observe).await
}

/// Whether a run holds its error lines back until its command printed (an `ssh` that may not
/// connect: its own complaint isn't the step's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hold {
    /// Every line is shown as it comes.
    No,
    /// Error lines wait for the first output line, or for the end.
    UntilOutput,
}

/// How one run of a step ended.
#[derive(Debug)]
pub(crate) struct Ended {
    /// Its exit, its last error lines, and its last [`STDOUT_REASON_LINES`] output lines (its
    /// lines were shown as they came: these are what its failure says).
    pub(crate) last: Captured,
    /// Whether its command printed anything on its standard output.
    pub(crate) printed: bool,
    /// Its verdict line, when the step's verdict found one.
    lead: Option<String>,
    /// The error lines held back (with [`Hold::UntilOutput`]), not shown yet.
    held: Vec<String>,
}

impl Ended {
    /// The step's outcome: its failure with the verdict, else why it failed
    /// ([`Captured::reason`]); a download's ([`Step::download`]) is `download_failed`.
    pub(crate) fn result(self, cx: &Cx<'_>, step: &Step) -> Result<()> {
        if self.last.success() {
            return Ok(());
        }
        let reason = cx.events.mask(&self.lead.unwrap_or_else(|| self.last.reason()));
        let id = step.id.to_string();
        Err(if step.download {
            Error::DownloadFailed { step: id, reason }
        } else {
            Error::ToolFailed { step: id, reason }
        })
    }

    /// Shows the lines held back, to `observe` too.
    pub(crate) fn show_held(&mut self, cx: &Cx<'_>, step: &Step, observe: &mut (dyn FnMut(&str) + Send)) {
        for line in std::mem::take(&mut self.held) {
            cx.events.log(step.id.as_str(), Stream::Stderr, &line);
            observe(&line);
        }
    }
}

/// Runs argv step `step` once, its lines shown (error lines held back with `hold`).
pub(crate) async fn once(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send), hold: Hold) -> Result<Ended> {
    let id = step.id.as_str();
    let spec = spec_for(cx, list, step)?;
    let verdict = match &step.verdict {
        Some(pattern) => Some(Regex::new(pattern).map_err(|e| Error::Malformed {
            at: format!("{}.verdict", plan::at(list, step)),
            reason: format!("`{pattern}` isn't a regular expression ({e})"),
        })?),
        None => None,
    };
    let (mut tail, mut lead, mut printed, mut kept_back) = (Tail::default(), None, false, Vec::new());
    let mut last_out = VecDeque::with_capacity(STDOUT_REASON_LINES);
    let progress = Progress::new();
    let mut on_line = |stream: Stream, line: &str| {
        progress.saw(line);
        if stream == Stream::Stderr {
            tail.push(line);
        } else {
            if last_out.len() == STDOUT_REASON_LINES {
                last_out.pop_front();
            }
            last_out.push_back(line.to_owned());
        }
        if hold == Hold::UntilOutput && !printed && stream == Stream::Stderr {
            kept_back.push(line.to_owned());
            return;
        }
        if stream == Stream::Stdout && !printed {
            printed = true;
            for earlier in kept_back.drain(..) {
                cx.events.log(id, Stream::Stderr, &earlier);
                observe(&earlier);
            }
        }
        cx.events.log(id, stream, line);
        observe(line);
        if let Some(found) = verdict.as_ref().and_then(|v| verdict_of(v, line)) {
            lead = Some(found);
        }
    };
    let run = cx.processes.run(&spec, &mut on_line);
    // A download runs while it makes progress, with no ceiling.
    let exit = if step.download {
        downloading(id, plan::limit(step, &cx.scope), &progress, run).await
    } else {
        timed(step, &cx.scope, &progress, run).await
    }
    .map_err(|e| masked(cx, e))?;
    Ok(Ended {
        last: Captured {
            exit,
            stdout: last_out.into(),
            stderr: tail.into_lines(),
        },
        printed,
        lead,
        held: kept_back,
    })
}

/// How one of a driver's own tool calls ran (an action's reads and clean-ups).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Echo {
    /// Its lines are kept, not shown.
    Quiet,
    /// Its lines are also `log` events of the action's step (a long clean-up, a boot).
    Shown,
}

/// Runs `spec` for the step `id`, as a driver's own call, until it shows no progress for `limit`
/// ([`within`]): its standard output and last error lines are kept, its exit status returned (a
/// failure is the caller's to judge). A missing tool and a timeout are errors.
pub(crate) async fn call(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    let progress = Progress::new();
    let mut shown = |stream: Stream, line: &str| {
        progress.saw(line);
        if echo == Echo::Shown {
            cx.events.log(id, stream, line);
        }
    };
    within(id, limit, &progress, capture_echoing(cx.processes.as_ref(), spec, &mut shown))
        .await
        .map_err(|e| masked(cx, e))
}

/// Runs `spec`, a download for step `id` made before the step it is for, its lines shown as
/// `log` events of `id`, until it shows no progress for `stall` ([`downloading`]: no ceiling).
/// Its standard output and last error lines are kept, its exit status returned (a failure is the
/// caller's to judge). A missing tool and a stall are errors.
pub(crate) async fn download(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, stall: Duration) -> Result<Captured> {
    let progress = Progress::new();
    let mut shown = |stream: Stream, line: &str| {
        progress.saw(line);
        cx.events.log(id, stream, line);
    };
    downloading(id, stall, &progress, capture_echoing(cx.processes.as_ref(), spec, &mut shown))
        .await
        .map_err(|e| masked(cx, e))
}

/// `e`, the last line a timeout or a stalled download quotes masked.
fn masked(cx: &Cx<'_>, e: Error) -> Error {
    match e {
        Error::Timeout { step, timed_out } => Error::Timeout {
            step,
            timed_out: timed_out.masked(|l| cx.events.mask(l)),
        },
        Error::DownloadStalled { step, after_s, last_line } => Error::DownloadStalled {
            step,
            after_s,
            last_line: last_line.map(|l| cx.events.mask(&l)),
        },
        other => other,
    }
}

/// [`call`], failing unless it exits 0: its standard output.
pub(crate) async fn call_ok(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<String> {
    let out = call(cx, id, spec, limit, echo).await?;
    stdout_of(cx, id, Some(spec), &out)
}

/// The standard output of a tool run for the step `id` that ended as `out`; unless it exited 0,
/// the step's failure, saying why ([`Captured::reason`]), led by the call's command line when it
/// was one of a driver's own calls (`call`), not the step itself.
pub(crate) fn stdout_of(cx: &Cx<'_>, id: &str, call: Option<&ProcessSpec>, out: &Captured) -> Result<String> {
    out.output().map_err(|why| {
        let reason = match call {
            Some(spec) => format!("`{}` failed: {why}", spec.line()),
            None => why,
        };
        Error::ToolFailed {
            step: id.to_owned(),
            reason: cx.events.mask(&reason),
        }
    })
}

/// Runs argv step `step` without events, returning its standard output: a read-only probe
/// (`lifecycle.status`).
pub(crate) async fn capture_step(cx: &Cx<'_>, list: &str, step: &Step) -> Result<String> {
    let out = capture_output(cx, list, step).await?;
    stdout_of(cx, step.id.as_str(), None, &out)
}

/// Runs argv step `step` without events: how it ended, a failure included (the caller's to
/// judge). A missing tool and a timeout are errors.
pub(crate) async fn capture_output(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Captured> {
    let spec = spec_for(cx, list, step)?;
    let progress = Progress::new();
    let run = progress.capture(cx.processes.as_ref(), &spec);
    timed(step, &cx.scope, &progress, run).await.map_err(|e| masked(cx, e))
}

/// The process for argv step `step`: its argv for this host, filled; its folder, environment and
/// input.
pub(crate) fn spec_for(cx: &Cx<'_>, list: &str, step: &Step) -> Result<ProcessSpec> {
    let at = plan::at(list, step);
    let argv = match &step.run {
        Run::Argv(argv) => argv,
        Run::Action { action: Action::SshRun, with } => return crate::ssh::step_spec(cx, list, step, with),
        Run::Action { .. } => {
            return Err(Error::Unsupported {
                what: format!("{at}: an action where a command was expected"),
            });
        }
    };
    let command = plan::command(argv, cx.scope.host, &cx.vars, &at)?;
    let dir = cx.run.dir();
    let spec = ProcessSpec {
        program: command.program,
        args: command.args,
        cwd: Some(step.cwd.as_ref().map_or_else(|| dir.to_path_buf(), |c| dir.join(c.as_str()))),
        env: env_of(cx, list, step)?,
        stdin: stdin_of(cx, list, step)?,
        tools: None,
    };
    Ok(crate::container::wrap(cx, spec))
}

/// The file piped into step `step`: its `stdin` filled, from the manifest's folder (a file of
/// the run's state folder is absolute already).
pub(crate) fn stdin_of(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Option<std::path::PathBuf>> {
    let Some(path) = &step.stdin else { return Ok(None) };
    let filled = cx.vars.render(path.template(), &format!("{}.stdin", plan::at(list, step)))?;
    Ok(Some(cx.run.dir().join(filled)))
}

/// Step `step`'s environment: the driver's, its own variables filled, then what it is delivered
/// ([`with_delivered`]).
pub(crate) fn env_of(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Vec<(String, String)>> {
    let at = plan::at(list, step);
    let mut env = cx.run.driver.env(cx)?;
    for (name, value) in &step.env {
        env.push((name.to_string(), cx.vars.render(value, &at)?));
    }
    Ok(with_delivered(cx, step.id.as_str(), env))
}

/// `env` (the driver's variables, a step's own), then the params and secrets delivered to step
/// `id`, then the relays': what a tool of the step runs with, a later variable winning (the
/// relays point the tools at loopback ports in place of the LAN host).
pub(crate) fn with_delivered(cx: &Cx<'_>, id: &str, mut env: Vec<(String, String)>) -> Vec<(String, String)> {
    env.extend(step_env(cx, id));
    env.extend(cx.relayed.iter().flat_map(|r| r.env.iter().cloned()));
    env
}

/// `run` given up once it shows no progress for `step`'s time limit in `scope`, or at the
/// ceiling ([`within`]); `progress` is fed by its line handler.
async fn timed<T>(step: &Step, scope: &Scope, progress: &Progress, run: impl Future<Output = Result<T, ProcessError>>) -> Result<T> {
    within(step.id.as_str(), plan::limit(step, scope), progress, run).await
}

/// A tool that couldn't run to its end, as step `step`'s failure.
pub(super) fn process_error(step: &str, e: ProcessError) -> Error {
    match e {
        ProcessError::Missing { program } => Error::ToolMissing {
            step: step.to_owned(),
            tool: program,
        },
        ProcessError::Elevation { reason } => Error::Elevation { reason },
        other => Error::ToolFailed {
            step: step.to_owned(),
            reason: other.to_string(),
        },
    }
}

/// How long a verdict leading a step's error may be.
const VERDICT_MAX: usize = 300;

/// The part of `line` the step's `verdict` matches, to the line's end (Vagrant's `<machine>: `
/// before Ansible's `fatal: [ws01]: …` left out), cut to [`VERDICT_MAX`] bytes on a character
/// boundary.
fn verdict_of(verdict: &Regex, line: &str) -> Option<String> {
    let found = verdict.find(line)?;
    Some(cut(line.get(found.start()..)?.trim_end(), VERDICT_MAX))
}

/// `text`, cut to `max` bytes (at least 3) on a character boundary, `...` included.
pub(super) fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let end = (0..=max.saturating_sub(3)).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
    format!("{}...", text.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_verdict_keeps_the_fatal_line_without_vagrants_prefix_and_short() {
        // Regression: a failed play only said "the SSH command responded with a non-zero exit
        // status"; the reason is Ansible's line, which Vagrant prefixes with the machine.
        let v = Regex::new("fatal: \\[[^\\]]+\\]: (UNREACHABLE|FAILED)!.*").unwrap();
        let line = "    isoloom-controller: fatal: [ws01]: UNREACHABLE! => {\"msg\": \"winrm connection error\"}";
        assert_eq!(
            verdict_of(&v, line).as_deref(),
            Some("fatal: [ws01]: UNREACHABLE! => {\"msg\": \"winrm connection error\"}")
        );
        assert_eq!(verdict_of(&v, "    dc01: ok: [dc01]"), None);
        let ascii = format!("fatal: [a]: FAILED! => {}", "x".repeat(400));
        assert_eq!(verdict_of(&v, &ascii).map(|c| c.len()), Some(VERDICT_MAX), "cut at the limit, `...` included");
        let long = format!("fatal: [a]: FAILED! => {}", "é".repeat(300));
        let cut = verdict_of(&v, &long).unwrap();
        assert!(cut.ends_with("...") && cut.len() <= VERDICT_MAX, "{} bytes", cut.len());
    }
}
