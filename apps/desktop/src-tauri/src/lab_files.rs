//! The files a lab's folder holds: isoloom.yml, Isoloom's generated files and the lab's own,
//! and the record of what the last run wrote, so a file it no longer needs goes away.

use std::path::{Component, Path};

use crate::runner::RunError;

/// The record of the files the last `prepare` wrote (one relative path per line).
const WRITTEN: &str = "written.list";

/// Whether `path` is relative and stays inside the lab's folder (no `..`, no root).
pub(crate) fn inside_lab(path: &str) -> bool {
    let p = Path::new(path);
    !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)))
}

/// Writes `files` (relative path, contents) into `dir`, removes those the last write had and
/// this one hasn't, and records the new list.
pub(crate) fn write_all(dir: &Path, files: &[(&str, &str)]) -> Result<(), RunError> {
    let paths: Vec<&str> = files.iter().map(|(p, _)| *p).collect();
    remove_stale(dir, &paths);
    for (path, contents) in files {
        write(dir, path, contents)?;
    }
    write(dir, WRITTEN, &paths.join("\n"))
}

/// Writes one file, making its folder.
pub(crate) fn write(dir: &Path, path: &str, contents: &str) -> Result<(), RunError> {
    let path = dir.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
    }
    std::fs::write(&path, contents).map_err(|e| io(&e))
}

/// Removes the files the last write recorded that `keep` doesn't name.
fn remove_stale(dir: &Path, keep: &[&str]) {
    let last = std::fs::read_to_string(dir.join(WRITTEN)).unwrap_or_default();
    for old in last.lines().filter(|p| inside_lab(p) && !keep.contains(p)) {
        drop(std::fs::remove_file(dir.join(old)));
    }
}

fn io(e: &std::io::Error) -> RunError {
    RunError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_lab() {
        assert!(inside_lab("configs/r1.cfg"));
        for bad in ["../escape.sh", "/etc/passwd", "a/../../b", "", "./x"] {
            assert!(!inside_lab(bad), "{bad}");
        }
    }

    #[test]
    fn a_file_the_lab_no_longer_has_goes_away() {
        let dir = std::env::temp_dir().join(format!("isothrum-files-{}", std::process::id()));
        write_all(&dir, &[("a/one.sh", "1"), ("two.sh", "2")]).unwrap();
        write_all(&dir, &[("two.sh", "2b")]).unwrap();
        assert!(!dir.join("a/one.sh").exists());
        assert_eq!(std::fs::read_to_string(dir.join("two.sh")).unwrap(), "2b");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
