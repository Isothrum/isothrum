//! `test`: the workspace's tests under cargo-nextest (each test in its own process, nothing
//! retried, slow tests reported: `.config/nextest.toml`), then the doctests, which nextest
//! doesn't run, with `cargo test --doc`. Both run, so a failure in one doesn't hide the other.
//!
//! xtask's ignored tests run too: those need cargo (`#[ignore = "needs cargo: ..."]`), which a
//! player's machine doesn't have, so the test programs run as they are on a fresh machine (the
//! products' workspace runs them in a VM) list them as ignored, with their reason, instead of
//! failing; here, with the toolchain, they run. The other crates' ignored tests stay opt-in (a
//! real run, an elevated prompt).
//!
//! Both run with a temp folder of their own (`TMPDIR`, `TMP` and `TEMP` name a fresh folder), and
//! anything a test leaves there fails the task: a test removes what it makes, and a leak caught
//! here never piles up in the developer's temp folder. The folder goes at the end, unless
//! `ISOTHRUM_KEEP_TEMP` is set: then it stays, and its path is printed, to look into what a test
//! left.
//!
//! That folder is the tests' alone, never the compiler wrapper's: the tests are built first with
//! the caller's own environment, then run with the folder and no wrapper. A wrapper can outlive
//! the run (sccache's server, which the first compile starts, keeps the temp folder it started
//! with), and one started with this folder would fail every later build once the folder is gone.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::SystemTime;

use crate::{Res, process, repo};

/// The variable that keeps the tests' temp folder after the run.
const KEEP: &str = "ISOTHRUM_KEEP_TEMP";

/// The arguments of `test`.
#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// More arguments for `cargo nextest run` (a filter, `--no-capture`); with them, the doctests
    /// don't run.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    nextest: Vec<String>,
}

/// Runs the tests, then the doctests; fails when either does, or when they left anything in
/// their temp folder.
pub(crate) fn run(a: &Args) -> Res<ExitCode> {
    let root = repo::root();
    process::require_cargo_tool(&root, "nextest")?;
    let temp = TempRoot::new()?;
    let built = process::passed(process::cargo(&root).args(["nextest", "run", "--workspace", "--no-run"]).args(&a.nextest))?;
    let tests = built && process::passed(temp.apply(process::cargo(&root).args(["nextest", "run", "--workspace"]).args(&a.nextest)))?;
    let needs_cargo =
        !a.nextest.is_empty() || process::passed(temp.apply(process::cargo(&root).args(["nextest", "run", "-p", "xtask", "--run-ignored", "only"])))?;
    let doctests = !a.nextest.is_empty() || process::passed(temp.apply(process::cargo(&root).args(["test", "--workspace", "--doc"])))?;
    let clean = temp.finish(std::env::var_os(KEEP).is_some_and(|v| !v.is_empty()))?;
    Ok(process::exit_code(tests && needs_cargo && doctests && clean))
}

/// The variables that name the temp folder, on one platform or another.
const TEMP_VARS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];

/// The temp folder the tests run with, fresh for the run, and when it was made.
struct TempRoot(tempfile::TempDir, SystemTime);

impl TempRoot {
    /// A new folder under `/tmp` on Unix (a short path: a test that binds a Unix socket under its
    /// temp folder stays under the 104 bytes macOS allows), under the system's temp folder
    /// elsewhere.
    fn new() -> Res<TempRoot> {
        let base = if cfg!(unix) { PathBuf::from("/tmp") } else { std::env::temp_dir() };
        let dir = tempfile::Builder::new()
            .prefix("isothrum-test-")
            .tempdir_in(&base)
            .map_err(|e| format!("no temp folder for the tests in {}: {e}", base.display()))?;
        Ok(TempRoot(dir, SystemTime::now()))
    }

    /// `cmd` with its temp folder this one, whichever variable its platform reads, and no compiler
    /// wrapper (an empty `RUSTC_WRAPPER` says none, over cargo's configuration too), which would
    /// keep the folder past the run. Only the wrapper of every crate, the one a compiler cache is:
    /// an empty `RUSTC_WORKSPACE_WRAPPER` would build the workspace's crates over again.
    fn apply<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        for var in TEMP_VARS {
            cmd.env(var, self.0.path());
        }
        cmd.env("RUSTC_WRAPPER", "")
    }

    /// Says what the tests left, then removes the folder (or keeps it, when `keep`); whether
    /// nothing was left.
    fn finish(self, keep: bool) -> Res<bool> {
        let left = leftovers(self.0.path())?;
        for name in &left {
            let path = self.0.path().join(name);
            let (inside, when) = (contents(&path), when(&path, self.1));
            eprintln!("xtask: a test left `{name}` in its temp folder{inside}{when}: a test removes what it makes (a `tempfile::TempDir`)");
        }
        if keep {
            eprintln!("xtask: the tests' temp folder is kept ({KEEP}): {}", self.0.keep().display());
        } else if let Err(e) = self.0.close() {
            eprintln!("xtask: the tests' temp folder couldn't be removed: {e}");
        }
        Ok(left.is_empty())
    }
}

/// What the folder `dir` holds, as a test left it (a few paths in it, two levels deep), to tell
/// which test it was; empty when it is a file or empty.
fn contents(dir: &Path) -> String {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let below = std::fs::read_dir(entry.path()).into_iter().flatten().flatten();
        let inner: Vec<String> = below.map(|e| format!("{name}/{}", e.file_name().to_string_lossy())).collect();
        if inner.is_empty() {
            found.push(name);
        } else {
            found.extend(inner);
        }
    }
    found.sort();
    if found.is_empty() {
        return String::new();
    }
    let more = found.len().saturating_sub(6);
    found.truncate(6);
    let more = if more > 0 { format!(", and {more} more") } else { String::new() };
    format!(" (holding {}{more})", found.join(", "))
}

/// When `path` was made and last written, in seconds since the tests began at `began`: the tests
/// running then, by the run's timed lines, are the ones that may have left it.
fn when(path: &Path, began: SystemTime) -> String {
    let since = |t: SystemTime| t.duration_since(began).map_or(0.0, |d| d.as_secs_f64());
    let Ok(meta) = std::fs::metadata(path) else { return String::new() };
    let made = meta.created().or_else(|_| meta.modified()).map(since);
    let below = std::fs::read_dir(path).into_iter().flatten().flatten();
    let written = below
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .chain(meta.modified().ok())
        .max()
        .map(since);
    match (made, written) {
        (Ok(made), Some(written)) => format!(", made {made:.1} s into the tests and last written at {written:.1} s"),
        _ => String::new(),
    }
}

/// The names at the top of `dir`, sorted.
fn leftovers(dir: &Path) -> Res<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leftover_says_when_it_was_made_and_last_written_since_the_tests_began() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.db"), "").unwrap();
        let began = SystemTime::now() - std::time::Duration::from_secs(5);
        let said = when(dir.path(), began);
        assert!(said.starts_with(", made 5.") && said.contains("last written at 5."), "{said}");
        assert_eq!(when(&dir.path().join("gone"), began), "", "nothing to say of a path that isn't there");
    }

    #[test]
    fn what_the_tests_leave_in_their_temp_folder_fails_the_run_and_the_folder_goes() {
        let temp = TempRoot::new().unwrap();
        let path = temp.0.path().to_path_buf();
        assert_eq!(leftovers(&path).unwrap(), Vec::<String>::new());
        std::fs::create_dir(path.join("isothrum-b")).unwrap();
        std::fs::write(path.join(".tmpA"), "").unwrap();
        assert_eq!(leftovers(&path).unwrap(), [".tmpA", "isothrum-b"]);
        assert!(!temp.finish(false).unwrap(), "a leftover fails the run");
        assert!(!path.exists(), "the folder goes all the same");
    }

    #[test]
    fn nothing_left_passes_and_the_folder_can_be_kept() {
        let temp = TempRoot::new().unwrap();
        let path = temp.0.path().to_path_buf();
        assert!(path.starts_with(if cfg!(unix) { PathBuf::from("/tmp") } else { std::env::temp_dir() }));
        let mut cmd = Command::new("true");
        cmd.env("RUSTC_WRAPPER", "sccache");
        let vars: std::collections::BTreeMap<_, _> = temp
            .apply(&mut cmd)
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(Path::new).map(Path::to_path_buf)))
            .collect();
        assert_eq!(vars.len(), TEMP_VARS.len() + 1);
        isothrum_test_support::assert_every!(TEMP_VARS.iter(), |var: &&str| vars[*var].as_deref() == Some(path.as_path()));
        assert_eq!(vars["RUSTC_WRAPPER"].as_deref(), Some(Path::new("")), "no compiler wrapper");
        assert!(temp.finish(true).unwrap(), "nothing left");
        assert!(path.exists(), "kept");
        std::fs::remove_dir(&path).unwrap();
    }
}
