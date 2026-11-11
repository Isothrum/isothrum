//! `test`: the workspace's tests under cargo-nextest (each test in its own process, nothing
//! retried, slow tests reported: `.config/nextest.toml`), then the doctests, which nextest
//! doesn't run, with `cargo test --doc`. Both run, so a failure in one doesn't hide the other.
//!
//! Both run with a temp folder of their own (`TMPDIR`, `TMP` and `TEMP` name a fresh folder), and
//! anything a test leaves there fails the task: a test removes what it makes, and a leak caught
//! here never piles up in the developer's temp folder. The folder goes at the end, unless
//! `ISOTHRUM_KEEP_TEMP` is set: then it stays, and its path is printed, to look into what a test
//! left.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

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
    let tests = process::passed(temp.apply(process::cargo(&root).args(["nextest", "run", "--workspace"]).args(&a.nextest)))?;
    let doctests = !a.nextest.is_empty() || process::passed(temp.apply(process::cargo(&root).args(["test", "--workspace", "--doc"])))?;
    let clean = temp.finish(std::env::var_os(KEEP).is_some_and(|v| !v.is_empty()))?;
    Ok(process::exit_code(tests && doctests && clean))
}

/// The temp folder the tests run with, fresh for the run.
struct TempRoot(tempfile::TempDir);

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
        Ok(TempRoot(dir))
    }

    /// `cmd` with its temp folder this one, whichever variable its platform reads.
    fn apply<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        for var in ["TMPDIR", "TMP", "TEMP"] {
            cmd.env(var, self.0.path());
        }
        cmd
    }

    /// Says what the tests left, then removes the folder (or keeps it, when `keep`); whether
    /// nothing was left.
    fn finish(self, keep: bool) -> Res<bool> {
        let left = leftovers(self.0.path())?;
        for name in &left {
            eprintln!("xtask: a test left `{name}` in its temp folder: a test removes what it makes (a `tempfile::TempDir`)");
        }
        if keep {
            eprintln!("xtask: the tests' temp folder is kept ({KEEP}): {}", self.0.keep().display());
        } else if let Err(e) = self.0.close() {
            eprintln!("xtask: the tests' temp folder couldn't be removed: {e}");
        }
        Ok(left.is_empty())
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
        let vars: Vec<_> = temp
            .apply(&mut cmd)
            .get_envs()
            .map(|(k, v)| (k.to_owned(), v.map(Path::new).map(Path::to_path_buf)))
            .collect();
        assert_eq!(vars.len(), 3);
        assert!(vars.iter().all(|(_, v)| v.as_deref() == Some(path.as_path())), "{vars:?}");
        assert!(temp.finish(true).unwrap(), "nothing left");
        assert!(path.exists(), "kept");
        std::fs::remove_dir(&path).unwrap();
    }
}
