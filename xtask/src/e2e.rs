//! `e2e`: isothrum-machine against real tools and real servers (its ignored `real` tests, through
//! its public API), one tier at a time: `machine`, this computer's own tools, or `server`, the
//! test servers the `ISOTHRUM_TEST_*` variables name. Each test runs in a process of its own, its
//! output in a log of its own, and is recorded as one JSON line in the `--results` file (appended
//! to): `{"suite":"e2e","tier":…,"case":…,"result":"pass|fail|skip","reason":…,"seconds":…,"log":…}`.
//! A test skips, saying why, when its tool or server isn't there (a line [`SKIP`] starts); a
//! skip fails nothing. The task fails when a test fails.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;

use crate::{Res, process, repo};

/// What a test prints to say it skipped, then why (the tests' own `SKIP`).
const SKIP: &str = "isothrum-e2e: skip: ";

/// The longest reason recorded: a failure's first lines, not its whole log.
const MAX_REASON: usize = 400;

/// The tiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Tier {
    /// This computer's own tools.
    Machine,
    /// The test servers the settings name.
    Server,
}

impl Tier {
    /// Its name, which is also its tests' module.
    fn name(self) -> &'static str {
        match self {
            Tier::Machine => "machine",
            Tier::Server => "server",
        }
    }
}

/// The arguments of `e2e`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// The tier to run.
    #[arg(long, value_enum)]
    tier: Tier,
    /// The file each test's result is appended to, a JSON line each; its logs go beside it
    /// (default: the build folder's `e2e/`, no results file).
    #[arg(long)]
    results: Option<PathBuf>,
    /// Only the tests whose name holds this.
    filter: Option<String>,
}

/// How a test ended.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    result: &'static str,
    reason: String,
}

/// Builds the tests, then runs each of the tier's, recording each; fails when one failed.
pub(crate) fn run(a: &Args) -> Res<std::process::ExitCode> {
    let root = repo::root();
    process::run(real_tests(&root).arg("--no-run"))?;
    let listed = process::output(real_tests(&root).args(["--", "--ignored", "--list", "--format", "terse"]))?;
    let tests = tier_tests(&listed, a.tier, a.filter.as_deref());
    if tests.is_empty() {
        return Err(format!(
            "no {} test{}",
            a.tier.name(),
            a.filter.as_ref().map(|f| format!(" matches {f}")).unwrap_or_default()
        )
        .into());
    }
    let logs = match &a.results {
        Some(file) => file.parent().map(Path::to_path_buf).unwrap_or_default(),
        None => repo::target_dir(&root).join("e2e"),
    };
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let mut failed = 0;
    for test in &tests {
        let case = test.strip_prefix(&format!("{}::", a.tier.name())).unwrap_or(test);
        let log = logs.join(format!("isothrum-{}-{}.log", a.tier.name(), case.replace("::", ".")));
        let start = Instant::now();
        let passed = run_one(&root, test, &log)?;
        let outcome = outcome(passed, &std::fs::read_to_string(&log).unwrap_or_default());
        let ran = Ran {
            tier: a.tier,
            case,
            outcome,
            seconds: start.elapsed().as_secs(),
            log,
        };
        println!(
            "{:5} {case} ({} s){}",
            ran.outcome.result.to_uppercase(),
            ran.seconds,
            reason_shown(&ran.outcome.reason)
        );
        failed += usize::from(ran.outcome.result == "fail");
        if let Some(file) = &a.results {
            append(file, &ran)?;
        }
    }
    println!("{} tier: {} run, {failed} failed", a.tier.name(), tests.len());
    Ok(process::exit_code(failed == 0))
}

/// `cargo test` of the `real` tests.
fn real_tests(root: &Path) -> std::process::Command {
    let mut cmd = process::cargo(root);
    cmd.args(["test", "-q", "-p", "isothrum-machine", "--test", "real"]);
    cmd
}

/// Runs `test` alone, its output in `log`: whether it passed.
fn run_one(root: &Path, test: &str, log: &Path) -> Res<bool> {
    let out = File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone()?;
    let mut cmd = real_tests(root);
    cmd.args(["--", "--ignored", "--exact", "--nocapture", test])
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    process::passed(&mut cmd)
}

/// The tier's tests in `cargo test --list --format terse`'s `listed` (`<name>: test` lines), those
/// whose name holds `filter` when there is one.
fn tier_tests(listed: &str, tier: Tier, filter: Option<&str>) -> Vec<String> {
    let prefix = format!("{}::", tier.name());
    listed
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .filter(|name| name.starts_with(&prefix) && filter.is_none_or(|f| name.contains(f)))
        .map(str::to_owned)
        .collect()
}

/// How a test ended, from whether it `passed` and its `log`: a skip when it said one, else a pass;
/// a failure's reason its panic's message.
fn outcome(passed: bool, log: &str) -> Outcome {
    if !passed {
        return Outcome {
            result: "fail",
            reason: panic_message(log).unwrap_or_else(|| "it failed without a panic (see its log)".into()),
        };
    }
    match log.lines().find_map(|l| l.strip_prefix(SKIP)) {
        Some(why) => Outcome {
            result: "skip",
            reason: why.trim().to_owned(),
        },
        None => Outcome {
            result: "pass",
            reason: String::new(),
        },
    }
}

/// The message of the first panic in `log`: the lines after `panicked at …:` up to the backtrace
/// note or a blank line, cut to [`MAX_REASON`].
fn panic_message(log: &str) -> Option<String> {
    let mut lines = log.lines().skip_while(|l| !l.contains("panicked at")).skip(1);
    let message: Vec<&str> = lines.by_ref().take_while(|l| !l.trim().is_empty() && !l.starts_with("note: ")).collect();
    let text = message.join(" ");
    (!text.is_empty()).then(|| text.chars().take(MAX_REASON).collect())
}

/// ` (reason)` to show after a result, nothing when there is none.
fn reason_shown(reason: &str) -> String {
    if reason.is_empty() { String::new() } else { format!(": {reason}") }
}

/// A test run: its tier and case, how it ended, how long it took and its log.
struct Ran<'a> {
    tier: Tier,
    case: &'a str,
    outcome: Outcome,
    seconds: u64,
    log: PathBuf,
}

impl Ran<'_> {
    /// Its record, the JSON line the results file holds.
    fn record(&self) -> serde_json::Value {
        serde_json::json!({
            "suite": "e2e",
            "tier": self.tier.name(),
            "case": self.case,
            "result": self.outcome.result,
            "reason": self.outcome.reason,
            "seconds": self.seconds,
            "log": self.log.display().to_string(),
        })
    }
}

/// Appends `ran`'s record to `file`.
fn append(file: &Path, ran: &Ran<'_>) -> Res<()> {
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    writeln!(f, "{}", ran.record())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tiers_tests_are_those_of_its_module_filtered_by_name() {
        let listed = "machine::net::a_port: test\nmachine::docker::start: test\nserver::ssh::keys: test\n\n2 tests, 0 benchmarks\n";
        assert_eq!(tier_tests(listed, Tier::Machine, None), ["machine::net::a_port", "machine::docker::start"]);
        assert_eq!(tier_tests(listed, Tier::Machine, Some("docker")), ["machine::docker::start"]);
        assert_eq!(tier_tests(listed, Tier::Server, None), ["server::ssh::keys"]);
    }

    #[test]
    fn a_test_passes_skips_saying_why_or_fails_with_its_panics_message() {
        let skipped = format!("running 1 test\n{SKIP}Docker's CLI isn't installed here\ntest x ... ok\n");
        assert_eq!(
            outcome(true, &skipped),
            Outcome {
                result: "skip",
                reason: "Docker's CLI isn't installed here".into()
            }
        );
        assert_eq!(outcome(true, "running 1 test\ntest x ... ok\n").result, "pass");
        let panicked = "\nthread 'x' panicked at tests/real/x.rs:3:5:\nassertion `left == right` failed: the key\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1`\n";
        assert_eq!(
            outcome(false, panicked),
            Outcome {
                result: "fail",
                reason: "assertion `left == right` failed: the key   left: 1  right: 2".into()
            }
        );
        assert_eq!(outcome(false, "error: could not compile").reason, "it failed without a panic (see its log)");
    }

    #[test]
    fn a_runs_record_is_the_line_the_products_workspace_reads() {
        let ran = Ran {
            tier: Tier::Server,
            case: "ssh::keys",
            outcome: outcome(true, &format!("{SKIP}not set: X\n")),
            seconds: 2,
            log: PathBuf::from("logs/x.log"),
        };
        let record = ran.record();
        assert_eq!(
            (
                record["tier"].as_str(),
                record["case"].as_str(),
                record["result"].as_str(),
                record["reason"].as_str()
            ),
            (Some("server"), Some("ssh::keys"), Some("skip"), Some("not set: X"))
        );
        assert_eq!((record["seconds"].as_u64(), record["suite"].as_str()), (Some(2), Some("e2e")));
    }
}
