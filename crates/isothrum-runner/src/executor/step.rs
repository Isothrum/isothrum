//! One argv step: the command filled in for this host and run, its lines streamed as `log`
//! events (masked), stopped with everything it started when its time is up.

use std::time::Duration;

use isoloom_manifest::{Action, Run, Step};
use regex::Regex;

use super::delivery::step_env;
use super::{Cx, plan};
use crate::Result;
use crate::error::Error;
use crate::events::Stream;
use crate::process::{Captured, Exit, ProcessError, ProcessSpec, Tail, capture};
use crate::scope::Scope;

/// Runs argv step `step` of list `list`; `observe` sees each line too (a check runner's results).
/// An `ssh.run` with a `wait_s` is tried again while its host doesn't answer
/// ([`super::reconnect`]).
pub(crate) async fn run_argv(cx: &Cx<'_>, list: &str, step: &Step, observe: &mut (dyn FnMut(&str) + Send)) -> Result<()> {
    if let Some(wait) = crate::ssh::wait_of(cx, list, step)? {
        return super::reconnect::run(cx, list, step, observe, &wait).await;
    }
    once(cx, list, step, observe, Hold::No).await?.result(cx, step)
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
    /// Its exit.
    pub(crate) exit: Exit,
    /// Whether its command printed anything on its standard output.
    pub(crate) printed: bool,
    /// Its verdict line, when the step's verdict found one.
    lead: Option<String>,
    /// Its last error lines.
    pub(crate) tail: Vec<String>,
    /// The error lines held back (with [`Hold::UntilOutput`]), not shown yet.
    held: Vec<String>,
}

impl Ended {
    /// The step's outcome: its failure with the verdict, else its last error lines.
    pub(crate) fn result(self, cx: &Cx<'_>, step: &Step) -> Result<()> {
        if self.exit.success() {
            return Ok(());
        }
        let reason = self.lead.unwrap_or_else(|| failure_text(&self.tail, self.exit));
        Err(Error::ToolFailed {
            step: step.id.to_string(),
            reason: cx.events.mask(&reason),
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
        Some(pattern) => Some(Regex::new(pattern).map_err(|e| Error::Unsupported {
            what: format!("step `{id}`'s verdict `{pattern}` ({e})"),
        })?),
        None => None,
    };
    let (mut tail, mut lead, mut printed, mut kept_back) = (Tail::default(), None, false, Vec::new());
    let mut on_line = |stream: Stream, line: &str| {
        if stream == Stream::Stderr {
            tail.push(line);
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
    let exit = timed(step, &cx.scope, cx.processes.run(&spec, &mut on_line)).await?;
    Ok(Ended {
        exit,
        printed,
        lead,
        tail: tail.into_lines(),
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

/// Runs `spec` for the step `id` within `limit`, as a driver's own call: its standard output and
/// last error lines are kept, its exit status returned (a failure is the caller's to judge). A
/// missing tool and a timeout are errors.
pub(crate) async fn call(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<Captured> {
    let mut stdout = Vec::new();
    let mut tail = Tail::default();
    let mut keep = |stream: Stream, line: &str| {
        if echo == Echo::Shown {
            cx.events.log(id, stream, line);
        }
        match stream {
            Stream::Stdout => stdout.push(line.to_owned()),
            Stream::Stderr => tail.push(line),
        }
    };
    let run = cx.processes.run(spec, &mut keep);
    let exit = tokio::time::timeout(limit, run).await.map_err(|_| Error::Timeout {
        step: id.to_owned(),
        after_s: limit.as_secs(),
    })?;
    let exit = exit.map_err(|e| process_error(id, e))?;
    Ok(Captured {
        stdout,
        stderr: tail.into_lines(),
        exit: Some(exit),
    })
}

/// [`call`], failing unless it exits 0: its standard output.
pub(crate) async fn call_ok(cx: &Cx<'_>, id: &str, spec: &ProcessSpec, limit: Duration, echo: Echo) -> Result<String> {
    let out = call(cx, id, spec, limit, echo).await?;
    if out.exit.is_some_and(Exit::success) {
        return Ok(out.stdout.join("\n"));
    }
    let reason = format!("`{}` failed: {}", spec.line(), why_failed(&out));
    Err(Error::ToolFailed {
        step: id.to_owned(),
        reason: cx.events.mask(&reason),
    })
}

/// Runs argv step `step` without events, returning its standard output: a read-only probe
/// (`lifecycle.status`).
pub(crate) async fn capture_step(cx: &Cx<'_>, list: &str, step: &Step) -> Result<String> {
    let spec = spec_for(cx, list, step)?;
    let out = timed(step, &cx.scope, capture(cx.processes.as_ref(), &spec)).await?;
    if !out.exit.is_some_and(Exit::success) {
        let reason = why_failed(&out);
        return Err(Error::ToolFailed {
            step: step.id.to_string(),
            reason: cx.events.mask(&reason),
        });
    }
    Ok(out.stdout.join("\n"))
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
    let command = plan::command(argv, cx.run.runner.config.host, &cx.vars, &at)?;
    let dir = cx.run.dir();
    let spec = ProcessSpec {
        program: command.program,
        args: command.args,
        cwd: Some(step.cwd.as_ref().map_or_else(|| dir.to_path_buf(), |c| dir.join(c.as_str()))),
        env: env_of(cx, list, step)?,
        stdin: stdin_of(cx, list, step)?,
    };
    crate::container::wrap(cx, spec)
}

/// The file piped into step `step`: its `stdin` filled, from the manifest's folder (a file of
/// the run's state folder is absolute already).
pub(crate) fn stdin_of(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Option<std::path::PathBuf>> {
    let Some(path) = &step.stdin else { return Ok(None) };
    let filled = cx.vars.render(path.template(), &format!("{}.stdin", plan::at(list, step)))?;
    Ok(Some(cx.run.dir().join(filled)))
}

/// Step `step`'s environment: its own variables, filled, then the params and secrets delivered
/// to it.
pub(crate) fn env_of(cx: &Cx<'_>, list: &str, step: &Step) -> Result<Vec<(String, String)>> {
    let at = plan::at(list, step);
    let mut env = cx.run.driver.env(cx)?;
    for (name, value) in &step.env {
        env.push((name.to_string(), cx.vars.render(value, &at)?));
    }
    env.extend(step_env(cx, step.id.as_str()));
    // The relays win: they point the tools at loopback ports in place of the LAN host.
    env.extend(cx.relayed.iter().flat_map(|r| r.env.iter().cloned()));
    Ok(env)
}

/// `run` given up after `step`'s time limit in `scope` (dropping it ends the process tree).
async fn timed<T>(step: &Step, scope: &Scope, run: impl Future<Output = Result<T, ProcessError>>) -> Result<T> {
    let secs = plan::limit_s(step, scope);
    let done = tokio::time::timeout(Duration::from_secs(secs), run).await.map_err(|_| Error::Timeout {
        step: step.id.to_string(),
        after_s: secs,
    })?;
    done.map_err(|e| process_error(step.id.as_str(), e))
}

/// A tool that couldn't run to its end, as step `step`'s failure.
fn process_error(step: &str, e: ProcessError) -> Error {
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
    let rest = line.get(found.start()..)?.trim_end();
    if rest.len() <= VERDICT_MAX {
        return Some(rest.to_owned());
    }
    let cut = (0..=VERDICT_MAX - 3).rev().find(|&i| rest.is_char_boundary(i)).unwrap_or(0);
    Some(format!("{}...", rest.get(..cut).unwrap_or_default()))
}

/// How many of its last output lines stand in for a failed tool's silent standard error.
const STDOUT_REASON_LINES: usize = 5;

/// Why `out` failed: its last error lines, else its last output lines (a tool, or a cloud image
/// refusing a login, may print its refusal there), with how it ended.
pub(crate) fn why_failed(out: &Captured) -> String {
    let quiet = out.stderr.iter().all(|l| l.trim().is_empty());
    let said = if quiet {
        out.stdout.get(out.stdout.len().saturating_sub(STDOUT_REASON_LINES)..).unwrap_or_default()
    } else {
        &out.stderr[..]
    };
    failure_text(said, out.exit.unwrap_or(Exit::new(None)))
}

/// What a failed tool's error says: its last stderr lines, else its exit status.
fn failure_text(tail: &[String], exit: Exit) -> String {
    let status = exit.code.map_or_else(|| "a signal".to_owned(), |c| format!("code {c}"));
    let cause = tail.join("\n");
    if cause.trim().is_empty() {
        format!("exited with {status}")
    } else {
        format!("{} (exited with {status})", cause.trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended(code: Option<i32>, stderr: &[&str], stdout: &[&str]) -> Captured {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect();
        Captured {
            exit: Some(Exit::new(code)),
            stdout: lines(stdout),
            stderr: lines(stderr),
        }
    }

    #[test]
    fn a_failure_says_why_from_stderr_else_the_end_of_stdout() {
        assert_eq!(why_failed(&ended(Some(1), &["denied"], &["noise"])), "denied (exited with code 1)");
        // Debian on AWS refuses root on stdout, with nothing on stderr.
        let refused = ended(Some(1), &["", " "], &["Please login as the user \"admin\" rather than the user \"root\"."]);
        assert_eq!(
            why_failed(&refused),
            "Please login as the user \"admin\" rather than the user \"root\". (exited with code 1)"
        );
        let long: Vec<String> = (1..=9).map(|n| format!("line {n}")).collect();
        let long: Vec<&str> = long.iter().map(String::as_str).collect();
        assert_eq!(
            why_failed(&ended(Some(2), &[], &long)),
            "line 5\nline 6\nline 7\nline 8\nline 9 (exited with code 2)"
        );
        assert_eq!(why_failed(&ended(None, &[], &[])), "exited with a signal");
    }

    #[test]
    fn a_silent_failure_reports_its_exit_status() {
        assert_eq!(failure_text(&[], Exit { code: Some(1) }), "exited with code 1");
        assert_eq!(failure_text(&[" ".into()], Exit { code: None }), "exited with a signal");
        assert_eq!(
            failure_text(&["no space left".into()], Exit { code: Some(2) }),
            "no space left (exited with code 2)"
        );
    }

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
