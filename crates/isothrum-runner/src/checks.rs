//! The run's checks: the manifest's `setup` steps, then each runner's step under its own timeout
//! (Isoloom computes it from the checks' waits, so a long-waiting run isn't cut off at a
//! fixed cap), its lines read with the `isoloom-check/1` grammar, then the
//! `teardown`. A check its runner never reported fails; a runner with no `END` is unfinished.

use isoloom_manifest::{Check, CheckRunner};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::error::Error;
use crate::events::EventKind;
use crate::events::WarningCode;
use crate::executor::{Cx, run_argv, run_steps};

/// One check's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckResult {
    /// The runner that ran it.
    pub runner: String,
    /// Where it ran from (`from web`).
    pub from: String,
    /// The check's name.
    pub name: String,
    /// Whether it passed.
    pub ok: bool,
    /// Why it failed (empty when it passed).
    pub detail: String,
}

/// Every check's outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CheckReport {
    /// The run has checks, every one ran, and every one passed.
    pub ok: bool,
    /// Every check, in the order it ran.
    pub results: Vec<CheckResult>,
    /// The runners that didn't run to their end (timed out, failed before `END`).
    pub unfinished: Vec<String>,
}

/// One line of a runner's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line {
    Pass(String),
    Fail(String, String),
    End(u32, u32),
}

/// Reads a runner's line, anywhere in it (tools prefix their output: `web-1  | isoloom-check: …`).
/// Ported from Isoloom's `isoloom-plan` (`checks/runner.rs`, `parse_line`).
pub(crate) fn parse_line(line: &str) -> Option<Line> {
    let (_, rest) = line.split_once("isoloom-check: ")?;
    if let Some(n) = rest.strip_prefix("PASS ") {
        return Some(Line::Pass(n.trim_end().to_owned()));
    }
    if let Some(r) = rest.strip_prefix("FAIL ") {
        let (name, why) = r.split_once(": ").unwrap_or((r, ""));
        return Some(Line::Fail(name.to_owned(), why.trim_end().to_owned()));
    }
    if let Some(r) = rest.strip_prefix("END ") {
        let mut nums = r.split_whitespace().filter_map(|w| w.parse::<u32>().ok());
        return Some(Line::End(nums.next()?, nums.next()?));
    }
    None
}

/// Runs the run's checks.
pub(crate) async fn run(cx: &mut Cx<'_>) -> Result<CheckReport> {
    let checks = &cx.run.manifest().checks;
    let mut report = CheckReport::default();
    let setup = run_steps(cx, "checks.setup", &checks.setup).await;
    if setup.is_ok() {
        for runner in &checks.runners {
            run_runner(cx, runner, &mut report).await;
        }
    }
    let teardown = run_steps(cx, "checks.teardown", &checks.teardown).await;
    setup?;
    teardown?;
    report.ok = !checks.runners.is_empty() && report.unfinished.is_empty() && report.results.iter().all(|r| r.ok);
    Ok(report)
}

/// Runs one check runner, reading its results into `report`.
async fn run_runner(cx: &mut Cx<'_>, runner: &CheckRunner, report: &mut CheckReport) {
    let id = runner.id.as_str();
    let before = report.results.len();
    let mut ended = false;
    let mut record = |line: &str| match parse_line(line) {
        Some(Line::Pass(name)) => push(cx, &mut report.results, result(runner, name, true, String::new())),
        Some(Line::Fail(name, why)) => {
            push(cx, &mut report.results, result(runner, name, false, why));
        }
        Some(Line::End(..)) => ended = true,
        None => {}
    };
    let ran = run_argv(cx, "checks.runners", &runner.step, &mut record).await;
    if let Err(e) = &ran {
        cx.events.warning(WarningCode::CheckRunnerFailed, &format!("{id}: {e}"));
        if matches!(e, Error::Timeout { .. }) {
            // A runner that hung outlives its CLI: remove what it left (`on_timeout`). Its steps
            // report their own failures; the runner's is the one the report holds.
            if let Err(cleanup) = run_steps(cx, "checks.on_timeout", &runner.on_timeout).await {
                cx.events.warning(WarningCode::CheckCleanupFailed, &format!("{id}: {cleanup}"));
            }
        }
    }
    let reported = report.results.len() - before;
    for check in runner.checks.iter().skip(reported) {
        push(cx, &mut report.results, unreported(runner, check));
    }
    if ran.is_err() || !ended {
        report.unfinished.push(id.to_owned());
    }
}

/// Keeps `r`, and emits it as it comes.
fn push(cx: &Cx<'_>, results: &mut Vec<CheckResult>, r: CheckResult) {
    cx.events.emit(EventKind::Check {
        runner: r.runner.clone(),
        from: r.from.clone(),
        name: r.name.clone(),
        ok: r.ok,
        detail: r.detail.clone(),
    });
    results.push(r);
}

fn result(runner: &CheckRunner, name: String, ok: bool, detail: String) -> CheckResult {
    CheckResult {
        runner: runner.id.to_string(),
        from: runner.from.clone(),
        name,
        ok,
        detail,
    }
}

/// A check its runner stopped before.
fn unreported(runner: &CheckRunner, check: &Check) -> CheckResult {
    result(runner, check.name.clone(), false, "the runner stopped before this check".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runner_lines_are_read_anywhere_in_a_line() {
        assert_eq!(parse_line("isoloom-check: PASS login works"), Some(Line::Pass("login works".into())));
        assert_eq!(
            parse_line("web-1  | isoloom-check: FAIL secret readable: 404"),
            Some(Line::Fail("secret readable".into(), "404".into()))
        );
        assert_eq!(parse_line("isoloom-check: FAIL no reason"), Some(Line::Fail("no reason".into(), String::new())));
        assert_eq!(parse_line("isoloom-check: END 3 passed, 1 failed"), Some(Line::End(3, 1)));
        assert_eq!(parse_line("isoloom-check: END soon"), None);
        assert_eq!(parse_line("some other output"), None);
    }
}

#[cfg(test)]
mod proptests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        #[test]
        fn any_line_is_read_without_panicking(line in "\\PC{0,120}") {
            let _ = parse_line(&line);
        }

        #[test]
        fn a_pass_reads_back_its_name_after_any_prefix(prefix in "[^i]{0,20}", name in "[a-zA-Z0-9][a-zA-Z0-9 /:._-]{0,40}[a-zA-Z0-9]") {
            prop_assert_eq!(parse_line(&format!("{prefix}isoloom-check: PASS {name}")), Some(Line::Pass(name)));
        }

        #[test]
        fn a_fail_reads_back_its_name_and_reason(name in "[a-z0-9 ._-]{1,30}", reason in "[a-zA-Z0-9 ]{0,30}[a-zA-Z0-9]") {
            prop_assert_eq!(parse_line(&format!("isoloom-check: FAIL {name}: {reason}")), Some(Line::Fail(name, reason)));
        }
    }
}
