//! The run's checks: the manifest's `setup` steps, then each runner's step under its own timeout
//! (Isoloom computes it from the checks' waits, so a long-waiting run isn't cut off at a
//! fixed cap), its lines read with the `isoloom-check/1` grammar ([`CheckLine::parse`]), then the
//! `teardown` (whose failure, after a setup that failed too, is a warning: the check ends with the
//! setup's). A check its runner never reported by name fails, whatever else it reported; a
//! runner with no `END` line (both counts in it) is unfinished.

use std::collections::BTreeSet;

use isoloom_manifest::{Check, CheckLine, CheckRunner};
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
    if let (Err(_), Err(e)) = (&setup, &teardown) {
        // The check ends with the setup's error: the teardown's is said, not dropped.
        cx.events.warning(WarningCode::CheckCleanupFailed, &format!("checks.teardown: {e}"));
    }
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
    let mut record = |line: &str| match CheckLine::parse(line) {
        Some(CheckLine::Pass { name }) => push(cx, &mut report.results, result(runner, name, true, String::new())),
        Some(CheckLine::Fail { name, reason }) => {
            push(cx, &mut report.results, result(runner, name, false, reason));
        }
        // Its counts aren't read: the report counts the results itself.
        Some(CheckLine::End { .. }) => ended = true,
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
    let reported: BTreeSet<String> = report.results.iter().skip(before).map(|r| r.name.clone()).collect();
    for check in runner.checks.iter().filter(|c| !reported.contains(&c.name)) {
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
