//! `isothrum reap`: the recorded runs past their lifetime removed, each by its `down` from the
//! folder it was started from, its tools given the values and secrets given here (an account's
//! region and keys). A failed removal doesn't stop the others; the command fails when one failed,
//! or when one is left (a run `--dry-run` would remove).

use std::time::SystemTime;

use isothrum_runner::{EventKind, Events, ResultData, RunOptions, RunRecord, Runner, SweepFailure};

use crate::args::ReapArgs;
use crate::commands::Out;
use crate::error::Result;
use crate::exit::Verdict;
use crate::term::Trail;
use crate::values::{self, Var};

/// What the sweep did with the expired runs.
#[derive(Debug, Default)]
struct Swept {
    /// Removed.
    destroyed: Vec<String>,
    /// Left for a later run (`--dry-run`).
    pending: Vec<String>,
    /// Not removed, and why.
    failed: Vec<(String, String)>,
}

/// Runs `args` on `runner`: its events and its summary to `out`; `var` reads the environment for
/// the secrets.
pub(crate) async fn run(args: &ReapArgs, runner: &Runner, out: &Out<'_>, var: &Var<'_>) -> Result<Verdict> {
    let Out { json, term, events } = *out;
    let mut opts = RunOptions::new().with_secrets(values::secrets(&args.secrets, var)?);
    for (name, value) in values::env(&args.env)? {
        opts = opts.with_env(name, value);
    }
    let expired = runner.expired(SystemTime::now())?;
    let mut swept = Swept::default();
    for record in &expired {
        let name = record.name.to_string();
        if args.dry_run {
            swept.pending.push(name);
            continue;
        }
        match destroy(runner, record, &opts, events).await {
            Ok(()) => swept.destroyed.push(name),
            Err(e) => swept.failed.push((name, e.to_string())),
        }
    }
    let clean = swept.failed.is_empty() && swept.pending.is_empty();
    if json {
        // The sweep is no one run's operation: its result has no `run` nor `op`, on a trail of
        // its own.
        let data = ResultData::Swept {
            destroyed: swept.destroyed.clone(),
            pending: swept.pending.clone(),
            failed: swept.failed.iter().map(|(run, why)| SweepFailure::new(run, why)).collect(),
        };
        term.json(&Trail::default().append(None, None, EventKind::Result { ok: clean, data }));
    } else {
        for line in lines(&swept) {
            term.out(&line);
        }
    }
    Ok(Verdict::of(clean))
}

/// The run of `record` removed: reopened from its recorded folder, then its `down`.
async fn destroy(runner: &Runner, record: &RunRecord, opts: &RunOptions, events: &dyn Events) -> isothrum_runner::Result<()> {
    runner.reopen(&record.name)?.down(opts, events).await
}

/// The sweep as a person reads it.
fn lines(swept: &Swept) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(swept.destroyed.iter().map(|r| format!("removed {r}")));
    out.extend(swept.pending.iter().map(|r| format!("would remove {r} (--dry-run)")));
    out.extend(swept.failed.iter().map(|(r, why)| format!("failed {r}: {why}")));
    if out.is_empty() {
        out.push("nothing to reap".to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    use isothrum_runner::fake::FakeProcesses;
    use isothrum_runner::{Adoption, RunName, RunnerConfig};

    use super::*;
    use crate::term::Term;
    use crate::testing::Buf;

    /// Regression: a dry run that found expired runs to remove passed, though they are left
    /// standing; the docs promise a failure then. With nothing expired, it passes. Nothing runs.
    #[tokio::test]
    async fn a_dry_run_fails_on_a_run_it_would_remove_and_passes_on_none() {
        let (home, module, deployment) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(module.path().join("main.tf"), "variable \"region\" {\n  default = \"eu\"\n}\n").unwrap();
        std::fs::write(
            deployment.path().join("terraform.tfstate"),
            r#"{"version":4,"serial":1,"lineage":"l","outputs":{},"resources":[]}"#,
        )
        .unwrap();
        let mut config = RunnerConfig::new(home.path());
        // Nothing scripted: a tool run fails the test.
        config.processes = Arc::new(FakeProcesses::script().build());
        let runner = Runner::new(config).unwrap();
        let said = Buf::default();
        let term = Term::new(Box::new(said.clone()), Box::new(Buf::default()));
        let out = Out {
            json: false,
            term: &term,
            events: &|_: &isothrum_runner::Event| {},
        };
        let dry = ReapArgs {
            dry_run: true,
            env: Vec::new(),
            secrets: Vec::new(),
        };

        assert_eq!(run(&dry, &runner, &out, &|_| None).await.unwrap(), Verdict::Pass, "nothing expired");
        assert_eq!(said.take().trim(), "nothing to reap");

        let a_minute_ago = SystemTime::now() - Duration::from_secs(60);
        let unix = a_minute_ago.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        let adoption = Adoption::new(RunName::new("expired").unwrap(), deployment.path()).with_expires_at(unix);
        runner.adopt(module.path(), adoption).await.unwrap();
        assert_eq!(run(&dry, &runner, &out, &|_| None).await.unwrap(), Verdict::Fail, "the expired run is left");
        assert_eq!(said.take().trim(), "would remove expired (--dry-run)");
    }
}
