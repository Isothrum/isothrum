//! The phases of a cloud real run, as the runner's command line takes them, and their order:
//! up, status and check while the budget lasts, then down and reap whatever happened.

use std::path::Path;
use std::time::Duration;

use isothrum_runner::wire_name;

use super::{Account, CloudArgs, NAME};
use crate::Res;

/// What a phase does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Start the run.
    Up,
    /// Read its state.
    Status,
    /// Run its checks.
    Check,
    /// Destroy it.
    Down,
    /// Destroy any run of the cloud past its expiry; list what no run records.
    Reap,
}

impl Step {
    /// As the runner's command line names it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Step::Up => "up",
            Step::Status => "status",
            Step::Check => "check",
            Step::Down => "down",
            Step::Reap => "reap",
        }
    }

    /// Whether it runs whatever happened before it (the destroy).
    fn always(self) -> bool {
        matches!(self, Step::Down | Step::Reap)
    }
}

/// One phase: its step and the runner's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Phase {
    /// What it does.
    pub(crate) step: Step,
    /// The arguments of the runner's command line.
    pub(crate) args: Vec<String>,
}

/// How a phase went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ran {
    /// It succeeded.
    Ok,
    /// It failed, and why.
    Failed(String),
    /// It didn't start: the budget was spent.
    OverBudget,
    /// It didn't start: the start failed, so there was nothing to read or check.
    Skipped,
}

/// Runs the runner's command line.
pub(crate) trait Runs {
    /// Runs it with `args` to its end.
    fn run(&mut self, args: &[String]) -> Ran;
}

/// The params a module takes for its machines' size, the smallest set on them.
const SIZES: [&str; 6] = ["instance_type", "size", "vm_size", "machine_type", "shape", "type"];

/// The phases on `run` (`.isoloom/<target>/<cloud>/run.json`) with the runner's home `home`:
/// `account`'s params and slots that the generated manifest `declared` takes, the smallest size,
/// the firewall open to `cidr`.
///
/// # Errors
///
/// The manifest isn't JSON, or it needs a param the account can't give.
pub(crate) fn phases(a: &CloudArgs, account: &Account, declared: &str, cidr: &str, (run_dir, home): (&Path, &Path)) -> Res<Vec<Phase>> {
    let manifest: serde_json::Value = serde_json::from_str(declared)?;
    let names = |key: &str| -> Vec<String> {
        let list = manifest.get(key).and_then(serde_json::Value::as_array);
        list.into_iter().flatten().filter_map(|p| p.get("name")?.as_str().map(str::to_owned)).collect()
    };
    let (params, slots) = (names("params"), names("secrets"));
    let mut given: Vec<(String, String)> = vec![("allowed_cidr".into(), cidr.into())];
    given.extend(
        SIZES
            .iter()
            .filter(|s| params.iter().any(|p| p == *s))
            .map(|s| ((*s).to_owned(), super::smallest(a.cloud).to_owned())),
    );
    given.extend(account.params.iter().filter(|(k, _)| params.contains(k)).cloned());
    if let Some(missing) = required(&manifest).into_iter().find(|r| !given.iter().any(|(k, _)| k == r)) {
        return Err(format!(
            "the {} manifest needs `{missing}`, which this run doesn't know how to give",
            wire_name(&a.cloud)
        )
        .into());
    }
    let secrets: Vec<&String> = account.secrets.iter().filter(|s| slots.contains(s)).collect();
    let common = |step: &str| -> Vec<String> {
        let target = format!("{}/{}", wire_name(&a.target), wire_name(&a.cloud));
        let run_dir = run_dir.display().to_string();
        let home = home.display().to_string();
        [step, &run_dir, "--target", &target, "--name", NAME, "--home", &home, "--json"]
            .map(str::to_owned)
            .to_vec()
    };
    let with_secrets = |mut args: Vec<String>| {
        args.extend(secrets.iter().flat_map(|s| ["--secret-from-env".to_owned(), (*s).clone()]));
        args
    };
    let mut up = with_secrets(common("up"));
    up.extend(["--lifetime".to_owned(), a.lifetime_minutes.to_string()]);
    up.extend(given.iter().flat_map(|(k, v)| ["--param".to_owned(), format!("{k}={v}")]));
    let mut reap: Vec<String> = vec!["reap".into(), wire_name(&a.cloud), "--home".into(), home.display().to_string(), "--json".into()];
    reap.extend(account.place.iter().cloned());
    Ok(vec![
        Phase { step: Step::Up, args: up },
        Phase {
            step: Step::Status,
            args: common("status"),
        },
        Phase {
            step: Step::Check,
            args: with_secrets(common("check")),
        },
        Phase {
            step: Step::Down,
            args: with_secrets(common("down")),
        },
        Phase {
            step: Step::Reap,
            args: with_secrets(reap),
        },
    ])
}

/// The params the manifest requires that have no default.
fn required(manifest: &serde_json::Value) -> Vec<String> {
    let list = manifest.get("params").and_then(serde_json::Value::as_array);
    list.into_iter()
        .flatten()
        .filter(|p| p.get("required").and_then(serde_json::Value::as_bool) == Some(true))
        .filter_map(|p| p.get("name")?.as_str().map(str::to_owned))
        .collect()
}

/// Runs `phases` through `runs`: up, status and check while `elapsed()` is within `budget` (and
/// only after a start that succeeded), down and reap always. Each phase with how it went.
pub(crate) fn execute(phases: &[Phase], budget: Duration, runs: &mut dyn Runs, elapsed: &dyn Fn() -> Duration) -> Vec<(Phase, Ran)> {
    let mut out: Vec<(Phase, Ran)> = Vec::new();
    for phase in phases {
        let started = out.iter().find(|(p, _)| p.step == Step::Up).is_none_or(|(_, r)| *r == Ran::Ok);
        let ran = if phase.step.always() {
            runs.run(&phase.args)
        } else if !started {
            Ran::Skipped
        } else if elapsed() > budget {
            Ran::OverBudget
        } else {
            runs.run(&phase.args)
        };
        out.push((phase.clone(), ran));
    }
    out
}

#[cfg(test)]
mod tests;
