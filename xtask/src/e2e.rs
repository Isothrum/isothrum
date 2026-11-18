//! `e2e`: isothrum-machine against real tools and real servers (its ignored `real` tests, through
//! its public API), one tier at a time: `machine`, this computer's own tools, or `server`, the
//! test servers the `ISOTHRUM_TEST_*` variables name. Each test runs in a process of its own, its
//! output in a log of its own, and is recorded as one JSON line in the `--results` file (appended
//! to): `{"suite":"e2e","tier":…,"case":…,"result":"pass|fail|skip","reason":…,"seconds":…,"log":…}`.
//! A test skips, saying why, when its tool or server isn't there (a line [`SKIP`] starts); a
//! skip fails nothing. The task fails when a test fails.
//!
//! On a machine without Rust's toolchain (a test VM), `--exe` names the `real` tests' executable,
//! built elsewhere (`cargo test --no-run`), which runs in place of `cargo test`.
//!
//! `--list` prints each test a tier has, a JSON line each (`{"suite":"e2e","tier":…,"case":…}`, as
//! its record names it), without building or running anything: read from the tests' sources (a
//! `#[ignore = "real …"]` test of `tests/real/<tier>/<module>.rs`), so a matrix of what is tested
//! knows each test on any computer. A run checks that list against the harness's own, so the two
//! can't drift.
//!
//! The installs run first (`<tier>::install::`): what they install stays, and the tests after them
//! run on it. An install a restart finishes (Hyper-V, WSL 2) says so (a line [`RESTART`] starts):
//! the run stops there, keeps where it is in a file beside the logs ([`Resume`]) and exits
//! [`RESTART_EXIT`]. Once the computer has restarted, the same command carries on: that test runs
//! again first, told it resumes (`ISOTHRUM_E2E_RESUMED=1`, so it checks what its install turned
//! on), then the tests not run yet. A test asking for a restart again on its resumption fails.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use crate::{Res, process, repo};

/// What a test prints to say it skipped, then why (the tests' own `SKIP`).
const SKIP: &str = "isothrum-e2e: skip: ";

/// What a test prints to say the computer must restart before it can go on, then why (the tests'
/// own `RESTART`).
const RESTART: &str = "isothrum-e2e: restart: ";

/// The variable a resumed test is run with.
const RESUMED: &str = "ISOTHRUM_E2E_RESUMED";

/// The exit code that asks for the computer restarted, then the same command run again
/// (`EX_TEMPFAIL`).
pub(crate) const RESTART_EXIT: u8 = 75;

/// The module whose tests run first.
const INSTALLS: &str = "install::";

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
    /// The tier to run (with `--list`, the tier listed; both without it).
    #[arg(long, value_enum, required_unless_present = "list")]
    tier: Option<Tier>,
    /// Print each test of the tier, a JSON line each, without building or running anything.
    #[arg(long)]
    pub(crate) list: bool,
    /// The file each test's result is appended to, a JSON line each; its logs go beside it
    /// (default: the build folder's `e2e/`, no results file).
    #[arg(long)]
    results: Option<PathBuf>,
    /// The `real` tests' executable, built already: run instead of building them with `cargo
    /// test` (a machine without Rust's toolchain).
    #[arg(long)]
    exe: Option<PathBuf>,
    /// Only the tests whose name holds this.
    filter: Option<String>,
}

impl Args {
    /// The tier a run runs: clap asks for one unless the tests are only listed.
    fn tier(&self) -> Tier {
        self.tier.unwrap_or(Tier::Machine)
    }
}

/// `--list`: each test of `a`'s tier (of both without one), a JSON line each, read from the
/// sources; nothing built or run.
pub(crate) fn list(a: &Args) -> Res<std::process::ExitCode> {
    let real = repo::root().join(REAL);
    for one in a.tier.map_or(vec![Tier::Machine, Tier::Server], |t| vec![t]) {
        for case in source_cases(&real, one)? {
            println!("{}", serde_json::json!({"suite": "e2e", "tier": one.name(), "case": case}));
        }
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// Fails unless the sources' list of `tier` (`--list`) is the harness's (`listed`): a test it
/// missed would be a test a matrix of what is tested can't see.
fn same_as_sources(root: &Path, tier: Tier, listed: &str) -> Res<()> {
    let prefix = format!("{}::", tier.name());
    let mut harnessed: Vec<String> = tier_tests(listed, tier, None)
        .iter()
        .map(|t| t.strip_prefix(&prefix).unwrap_or(t).to_owned())
        .collect();
    let mut read = source_cases(&root.join(REAL), tier)?;
    harnessed.sort();
    read.sort();
    if harnessed == read {
        return Ok(());
    }
    Err(format!(
        "`--list` reads other {} tests than the harness lists: {read:?} against {harnessed:?}",
        tier.name()
    )
    .into())
}

/// How a test ended.
#[derive(Debug, PartialEq, Eq)]
struct Outcome {
    result: &'static str,
    reason: String,
}

/// Builds the tests, then runs each of the tier's, recording each; fails when one failed. Stops at a
/// test that asks for a restart ([`RESTART_EXIT`]), and carries on from it at the next run.
pub(crate) fn run(a: &Args) -> Res<std::process::ExitCode> {
    let root = repo::root();
    let tier = a.tier();
    if a.exe.is_none() {
        process::run(real_tests(&root).arg("--no-run"))?;
    }
    let listed = process::output(&mut harness(&root, a.exe.as_deref(), &["--ignored", "--list", "--format", "terse"]))?;
    let tests = tier_tests(&listed, tier, a.filter.as_deref());
    if a.exe.is_none() {
        same_as_sources(&root, tier, &listed)?;
    }
    if tests.is_empty() {
        return Err(format!(
            "no {} test{}",
            tier.name(),
            a.filter.as_ref().map(|f| format!(" matches {f}")).unwrap_or_default()
        )
        .into());
    }
    let logs = match &a.results {
        Some(file) => file.parent().map(Path::to_path_buf).unwrap_or_default(),
        None => repo::target_dir(&root).join("e2e"),
    };
    std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
    let state = logs.join(format!("isothrum-e2e-{}.resume.json", tier.name()));
    let mut resume = Resume::read(&state)?;
    if let Some(case) = &resume.resume {
        println!("carrying on after a restart, from {case}");
    }
    let mut failed = 0;
    for test in resume.order(&tests) {
        let case = test.strip_prefix(&format!("{}::", tier.name())).unwrap_or(&test);
        let ran = run_test(a, &root, &logs, case, resume.resume.as_deref() == Some(case))?;
        if ran.outcome.result == "restart" {
            println!("RESTART {case}: {}", ran.outcome.reason);
            resume.resume = Some(case.to_owned());
            resume.write(&state)?;
            println!("restart this computer, then run the same command again: it carries on from {case}");
            return Ok(std::process::ExitCode::from(RESTART_EXIT));
        }
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
        resume.done(case);
        resume.write(&state)?;
    }
    match std::fs::remove_file(&state) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{}: {e}", state.display()).into()),
        _ => {}
    }
    println!("{} tier: {} run, {failed} failed", tier.name(), tests.len());
    Ok(process::exit_code(failed == 0))
}

/// Runs test `case` of the tier alone, its output in a log of its own in `logs`; `resumed`: it asked
/// for the restart the computer has since made. A resumed test asking for one again fails.
fn run_test<'a>(a: &Args, root: &Path, logs: &Path, case: &'a str, resumed: bool) -> Res<Ran<'a>> {
    let tier = a.tier();
    let log = logs.join(format!("isothrum-{}-{}.log", tier.name(), case.replace("::", ".")));
    let start = Instant::now();
    let test = format!("{}::{case}", tier.name());
    let mut cmd = harness(root, a.exe.as_deref(), &["--ignored", "--exact", "--nocapture", &test]);
    if resumed {
        cmd.env(RESUMED, "1");
    }
    let passed = run_one(cmd, &log)?;
    let mut outcome = outcome(passed, &std::fs::read_to_string(&log).unwrap_or_default());
    if resumed && outcome.result == "restart" {
        outcome = Outcome {
            result: "fail",
            reason: format!("it asks for a restart again once restarted: {}", outcome.reason),
        };
    }
    Ok(Ran {
        tier,
        case,
        outcome,
        seconds: start.elapsed().as_secs(),
        log,
    })
}

/// Where a run that stopped for a restart is: the cases it ran (recorded already) and the one it
/// stopped at, kept in a file beside its logs until the run ends.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Resume {
    /// The cases run, in order.
    done: Vec<String>,
    /// The case that asked for the restart.
    resume: Option<String>,
}

impl Resume {
    /// The one in `file`; a fresh one when there is none.
    fn read(file: &Path) -> Res<Resume> {
        match std::fs::read_to_string(file) {
            Ok(text) => Ok(serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Resume::default()),
            Err(e) => Err(format!("{}: {e}", file.display()).into()),
        }
    }

    /// Writes it to `file`.
    fn write(&self, file: &Path) -> Res<()> {
        std::fs::write(file, serde_json::to_string(self)?).map_err(|e| format!("{}: {e}", file.display()).into())
    }

    /// Case `case` ran (no longer the one resumed).
    fn done(&mut self, case: &str) {
        self.done.push(case.to_owned());
        if self.resume.as_deref() == Some(case) {
            self.resume = None;
        }
    }

    /// `tests` still to run: the one resumed first, then those not run yet, in order.
    fn order(&self, tests: &[String]) -> Vec<String> {
        let case = |t: &String| t.split_once("::").map_or(t.clone(), |(_, c)| c.to_owned());
        let resumed = tests.iter().filter(|t| self.resume.as_ref() == Some(&case(t)));
        let rest = tests.iter().filter(|t| self.resume.as_ref() != Some(&case(t)) && !self.done.contains(&case(t)));
        resumed.chain(rest).cloned().collect()
    }
}

/// `cargo test` of the `real` tests.
fn real_tests(root: &Path) -> Command {
    let mut cmd = process::cargo(root);
    cmd.args(["test", "-q", "-p", "isothrum-machine", "--test", "real"]);
    cmd
}

/// Where the `real` tests' sources are, from the workspace's root.
const REAL: &str = "crates/isothrum-machine/tests/real";

/// The tier's tests, as `<module>::<test>`, read from their sources in `real`: each function
/// after an `#[ignore = "real …"]` in `<tier>/<module>.rs`, by module in name order, each in the
/// order written.
fn source_cases(real: &Path, tier: Tier) -> Res<Vec<String>> {
    let dir = real.join(tier.name());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    let mut cases = Vec::new();
    for file in files {
        let module = file.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned();
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        cases.extend(ignored_tests(&text).into_iter().map(|name| format!("{module}::{name}")));
    }
    Ok(cases)
}

/// The functions of `source` that follow an `#[ignore = "real …"]`, in order.
fn ignored_tests(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut ignored = false;
    for line in source.lines().map(str::trim) {
        if line.starts_with("#[ignore = \"real ") {
            ignored = true;
        } else if ignored && let Some((_, after)) = line.split_once("fn ") {
            let name: String = after.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            names.push(name);
            ignored = false;
        }
    }
    names
}

/// The `real` tests' harness with its own arguments `args`: `exe` when given, else `cargo test`.
fn harness(root: &Path, exe: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = if let Some(exe) = exe {
        Command::new(exe)
    } else {
        let mut cmd = real_tests(root);
        cmd.arg("--");
        cmd
    };
    cmd.args(args);
    cmd
}

/// Runs `cmd` (one test, alone), its output in `log`: whether it passed.
fn run_one(mut cmd: Command, log: &Path) -> Res<bool> {
    let out = File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone()?;
    cmd.stdin(Stdio::null()).stdout(out).stderr(err);
    process::passed(&mut cmd)
}

/// The install tests that run after the other installs, in this order: libvirt's brings in
/// another's (QEMU), which would leave that one's install untested on a fresh machine ("installed
/// here already"); then the Vagrant plugins, which need Vagrant and their hypervisor installed
/// (vagrant-libvirt builds against libvirt's headers).
const INSTALLED_LAST: [&str; 2] = ["install::libvirt_", "install::plugin_"];

/// The tier's tests in `cargo test --list --format terse`'s `listed` (`<name>: test` lines), those
/// whose name holds `filter` when there is one: its installs first ([`INSTALLED_LAST`]'s last of
/// them), then the others, each in the order listed.
fn tier_tests(listed: &str, tier: Tier, filter: Option<&str>) -> Vec<String> {
    let prefix = format!("{}::", tier.name());
    let mut tests: Vec<String> = listed
        .lines()
        .filter_map(|l| l.strip_suffix(": test"))
        .filter(|name| name.starts_with(&prefix) && filter.is_none_or(|f| name.contains(f)))
        .map(str::to_owned)
        .collect();
    tests.sort_by_key(|name| {
        let case = name.strip_prefix(&prefix).unwrap_or(name);
        (
            !case.starts_with(INSTALLS),
            INSTALLED_LAST.iter().position(|last| case.starts_with(last)).map_or(0, |at| at + 1),
        )
    });
    tests
}

/// How a test ended, from whether it `passed` and its `log`: a skip or a restart when it said one,
/// else a pass; a failure's reason its panic's message.
fn outcome(passed: bool, log: &str) -> Outcome {
    if !passed {
        return Outcome {
            result: "fail",
            reason: panic_message(log).unwrap_or_else(|| "it failed without a panic (see its log)".into()),
        };
    }
    let said = |prefix: &str| log.lines().find_map(|l| l.strip_prefix(prefix)).map(|why| why.trim().to_owned());
    match (said(SKIP), said(RESTART)) {
        (Some(reason), _) => Outcome { result: "skip", reason },
        (None, Some(reason)) => Outcome { result: "restart", reason },
        (None, None) => Outcome {
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
    fn the_list_is_each_function_after_a_real_ignore_by_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("server")).unwrap();
        let ssh = "/// Keys.\n#[tokio::test]\n#[ignore = \"real server: cargo xtask e2e --tier server\"]\nasync fn keys_are_confirmed() {}\n\nfn helper() {}\n#[test]\n#[ignore = \"real server\"]\nfn a_password() {}\n";
        std::fs::write(dir.path().join("server/ssh.rs"), ssh).unwrap();
        std::fs::write(
            dir.path().join("server/esxi.rs"),
            "#[ignore = \"slow\"]\nfn not_real() {}\n#[ignore = \"real server\"]\nasync fn a_vm() {}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("server/notes.txt"), "#[ignore = \"real server\"]\nfn no() {}\n").unwrap();
        assert_eq!(
            source_cases(dir.path(), Tier::Server).unwrap(),
            ["esxi::a_vm", "ssh::keys_are_confirmed", "ssh::a_password"]
        );
    }

    #[test]
    fn the_workspaces_real_tests_are_listed() {
        let real = repo::root().join(REAL);
        let machine = source_cases(&real, Tier::Machine).unwrap();
        assert!(machine.iter().any(|c| c == "install::docker_is_installed_where_it_is_missing"), "{machine:?}");
        assert!(source_cases(&real, Tier::Server).unwrap().iter().any(|c| c.starts_with("ssh::")));
    }

    #[test]
    fn a_tiers_tests_are_those_of_its_module_filtered_by_name() {
        let listed = "machine::net::a_port: test\nmachine::docker::start: test\nserver::ssh::keys: test\n\n2 tests, 0 benchmarks\n";
        assert_eq!(tier_tests(listed, Tier::Machine, None), ["machine::net::a_port", "machine::docker::start"]);
        assert_eq!(tier_tests(listed, Tier::Machine, Some("docker")), ["machine::docker::start"]);
        assert_eq!(tier_tests(listed, Tier::Server, None), ["server::ssh::keys"]);
    }

    #[test]
    fn libvirts_install_runs_after_qemus_which_it_brings() {
        let listed = "machine::net::a: test\nmachine::install::docker_x: test\nmachine::install::libvirt_x: test\nmachine::install::qemu_x: test\n";
        assert_eq!(
            tier_tests(listed, Tier::Machine, None),
            [
                "machine::install::docker_x",
                "machine::install::qemu_x",
                "machine::install::libvirt_x",
                "machine::net::a"
            ]
        );
    }

    #[test]
    fn the_vagrant_plugins_install_last_after_libvirt() {
        let listed = "machine::install::plugin_vagrant_libvirt_x: test\nmachine::install::libvirt_x: test\nmachine::net::a: test\nmachine::install::vagrant_is_x: test\n";
        assert_eq!(
            tier_tests(listed, Tier::Machine, None),
            [
                "machine::install::vagrant_is_x",
                "machine::install::libvirt_x",
                "machine::install::plugin_vagrant_libvirt_x",
                "machine::net::a"
            ]
        );
    }

    #[test]
    fn the_installs_run_first_the_rest_in_the_order_listed() {
        let listed = "machine::cloud::token: test\nmachine::install::wsl: test\nmachine::docker::start: test\nmachine::install::docker: test\n";
        assert_eq!(
            tier_tests(listed, Tier::Machine, None),
            [
                "machine::install::wsl",
                "machine::install::docker",
                "machine::cloud::token",
                "machine::docker::start"
            ]
        );
    }

    #[test]
    fn a_test_asks_for_a_restart_saying_why() {
        let said = format!("running 1 test\n$ dism.exe\n{RESTART}Hyper-V starts at the next restart\ntest x ... ok\n");
        assert_eq!(
            outcome(true, &said),
            Outcome {
                result: "restart",
                reason: "Hyper-V starts at the next restart".into()
            }
        );
        assert_eq!(outcome(false, &said).result, "fail");
    }

    #[test]
    fn a_run_carries_on_from_the_test_that_asked_for_the_restart() {
        let tests = ["machine::install::a", "machine::install::b", "machine::install::c", "machine::net::d"].map(String::from);
        let mut resume = Resume::default();
        assert_eq!(resume.order(&tests), tests);
        resume.done("install::a");
        resume.resume = Some("install::b".into());
        assert_eq!(resume.order(&tests), ["machine::install::b", "machine::install::c", "machine::net::d"]);
        resume.done("install::b");
        assert_eq!(resume.resume, None);
        assert_eq!(resume.order(&tests), ["machine::install::c", "machine::net::d"]);

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("resume.json");
        assert_eq!(Resume::read(&file).unwrap(), Resume::default());
        resume.write(&file).unwrap();
        assert_eq!(Resume::read(&file).unwrap(), resume);
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
