//! Finding the programs the app runs: docker, vagrant, wireshark, a terminal.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Whether a program is on the PATH (on Windows with its extension: `vagrant` is `vagrant.exe`).
pub(crate) fn has(program: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let names = candidates(program, &extensions());
    std::env::split_paths(&paths).any(|dir| found_in(&dir, &names))
}

/// Whether one of `names` is a file in `dir`.
fn found_in(dir: &Path, names: &[String]) -> bool {
    names.iter().any(|n| dir.join(n).is_file())
}

/// Windows' executable extensions (`PATHEXT`); none elsewhere.
fn extensions() -> Vec<String> {
    if !cfg!(windows) {
        return vec![];
    }
    std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
        .split(';')
        .filter(|e| !e.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The file names `program` may have: itself, then with each extension when it has none.
fn candidates(program: &str, extensions: &[String]) -> Vec<String> {
    let mut names = vec![program.to_owned()];
    if Path::new(program).extension().is_none() {
        names.extend(extensions.iter().map(|e| format!("{program}{e}")));
    }
    names
}

/// Where the tools usually are when the PATH doesn't say: an app started from macOS's Finder gets
/// only `/usr/bin:/bin:/usr/sbin:/sbin`, and Wireshark's installer leaves Windows' PATH alone.
fn usual_dirs() -> Vec<PathBuf> {
    if cfg!(target_os = "macos") {
        [
            "/usr/local/bin",
            "/opt/homebrew/bin",
            "/Applications/Docker.app/Contents/Resources/bin",
            "/Applications/Wireshark.app/Contents/MacOS",
        ]
        .map(PathBuf::from)
        .to_vec()
    } else if cfg!(windows) {
        let programs =
            std::env::var_os("ProgramFiles").unwrap_or_else(|| "C:\\Program Files".into());
        vec![PathBuf::from(programs).join("Wireshark")]
    } else {
        vec![]
    }
}

/// `path` with each of `dirs` it lacks added at the end.
fn with_dirs(path: &OsString, dirs: &[PathBuf]) -> OsString {
    let mut all: Vec<PathBuf> = std::env::split_paths(path).collect();
    for d in dirs {
        if !all.contains(d) {
            all.push(d.clone());
        }
    }
    std::env::join_paths(all).unwrap_or_else(|_| path.clone())
}

/// Adds the tools' usual folders to this process's PATH, so a Finder-launched app (whose PATH is
/// the bare `/usr/bin:/bin:…`) and every tool it later spawns — `docker`, `vagrant`, the
/// `isothrum-machine` probes — still find them. Call it first, before any thread starts.
///
/// This is the one place the crate sets a process-wide environment variable: extending PATH in
/// place is the only way to reach both the direct `std::process::Command` spawns and the
/// `isothrum-machine` runner, which inherits the process environment and takes no PATH of its own.
/// The crate therefore denies (not forbids) `unsafe_code` so this single, audited use can carry an
/// `#[expect]`; everywhere else stays unsafe-free.
#[expect(
    unsafe_code,
    reason = "set_var is the only way to extend PATH for this process and every tool it spawns; \
              called once at start-up before any thread exists, so the documented safety holds"
)]
pub(crate) fn complete_path() {
    let current = std::env::var_os("PATH").unwrap_or_default();
    let path = with_dirs(&current, &usual_dirs());
    if path != current {
        // SAFETY: called once at start-up (from `lib::run`), before the app spawns any thread, so
        // no other thread can read or write the environment while it is being set.
        unsafe { std::env::set_var("PATH", path) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_may_need_an_extension() {
        let exts = [".exe".to_owned(), ".cmd".to_owned()];
        assert_eq!(
            candidates("vagrant", &exts),
            ["vagrant", "vagrant.exe", "vagrant.cmd"]
        );
        assert_eq!(candidates("vagrant.exe", &exts), ["vagrant.exe"]);
        assert_eq!(candidates("vagrant", &[]), ["vagrant"]);
    }

    #[test]
    fn a_program_is_found_with_its_extension() {
        let dir = std::env::temp_dir().join(format!("isothrum-tools-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("vagrant.exe"), "").unwrap();
        assert!(found_in(&dir, &candidates("vagrant", &[".exe".into()])));
        assert!(!found_in(&dir, &candidates("vagrant", &[])));
        assert!(!found_in(&dir, &candidates("wireshark", &[".exe".into()])));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_usual_folders_join_the_path_once() {
        let path = std::env::join_paths(["/usr/bin", "/opt/homebrew/bin"]).unwrap();
        let dirs = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
        let done = with_dirs(&path, &dirs);
        let all: Vec<PathBuf> = std::env::split_paths(&done).collect();
        assert_eq!(
            all,
            ["/usr/bin", "/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from)
        );
    }
}
