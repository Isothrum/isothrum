//! `isothrum reap <cloud>`: the recorded runs of a cloud past their expiry destroyed, each with
//! the params it was applied with and the connection given here (`--secret-from-env`); then
//! what the cloud still holds tagged past its expiry that no run covers, listed for a person to
//! remove. A failed destroy doesn't stop the others; the command fails when one failed, when
//! something is left, or when a query of the cloud couldn't run (nothing says it's clean).

use std::time::SystemTime;

use isothrum_runner::{Events, Orphans, Place, RunOptions, RunRecord, Runner, Secrets};
use serde_json::json;

use crate::args::ReapArgs;
use crate::commands::Out;
use crate::error::Result;
use crate::exit::Verdict;
use crate::values;

/// What the sweep did with the expired runs.
#[derive(Debug, Default)]
struct Swept {
    /// Destroyed.
    destroyed: Vec<String>,
    /// Left for a later run (`--dry-run`).
    pending: Vec<String>,
    /// Not destroyed, and why.
    failed: Vec<(String, String)>,
}

/// Runs `args` on `runner`: its events and its summary to `out`; `var` reads the environment for
/// the connection.
pub(crate) async fn run(args: &ReapArgs, runner: &Runner, out: &Out<'_>, var: &dyn Fn(&str) -> Option<String>) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    let cloud = args.cloud.into();
    let connection = values::secrets(&args.secrets, var)?;
    let now = SystemTime::now();
    let expired: Vec<RunRecord> = runner.expired(now)?.into_iter().filter(|r| r.cloud == Some(cloud)).collect();
    let mut swept = Swept::default();
    for record in &expired {
        let name = record.name.to_string();
        if args.dry_run {
            swept.pending.push(name);
            continue;
        }
        match destroy(runner, record, &connection, events).await {
            Ok(()) => swept.destroyed.push(name),
            Err(e) => swept.failed.push((name, e.to_string())),
        }
    }
    let orphans = runner.orphans(cloud, &Place::new(args.regions.clone(), args.project.clone()), now).await?;
    if json {
        term.out(&summary(args, &swept, &orphans).to_string());
    } else {
        for line in lines(args, &swept, &orphans) {
            term.out(&line);
        }
    }
    Ok(Verdict::of(
        swept.failed.is_empty() && orphans.resources.is_empty() && orphans.unread.is_empty(),
    ))
}

/// The run of `record` destroyed, with `connection`.
async fn destroy(runner: &Runner, record: &RunRecord, connection: &Secrets, events: &dyn Events) -> isothrum_runner::Result<()> {
    let run = runner.open(&record.manifest, Some(record.name.clone()))?.with_connection(connection.clone());
    run.down(&RunOptions::new(), events).await
}

/// The sweep as one JSON object.
fn summary(args: &ReapArgs, swept: &Swept, orphans: &Orphans) -> serde_json::Value {
    let failed: Vec<serde_json::Value> = swept.failed.iter().map(|(run, why)| json!({ "run": run, "error": why })).collect();
    json!({
        "cloud": args.cloud.id(),
        "destroyed": swept.destroyed,
        "pending": swept.pending,
        "failed": failed,
        "orphans": orphans.resources,
        "unread": orphans.unread,
    })
}

/// The sweep as a person reads it.
fn lines(args: &ReapArgs, swept: &Swept, orphans: &Orphans) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(swept.destroyed.iter().map(|r| format!("destroyed {r}")));
    out.extend(swept.pending.iter().map(|r| format!("would destroy {r} (--dry-run)")));
    out.extend(swept.failed.iter().map(|(r, why)| format!("failed {r}: {why}")));
    for o in &orphans.resources {
        let env = o.instance.as_deref().unwrap_or("unknown");
        out.push(format!(
            "left {} {} (environment {env}): no run records it; remove it in the cloud's console",
            o.kind, o.id
        ));
    }
    out.extend(orphans.unread.iter().map(|why| format!("not checked: {why}")));
    if out.is_empty() {
        out.push(format!("nothing to reap on {}", args.cloud.id()));
    }
    out
}

#[cfg(test)]
mod tests;
