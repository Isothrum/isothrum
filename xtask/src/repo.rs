//! Where things are: this checkout, its build folder, and paths as the tools read them.

use std::path::{Path, PathBuf};

/// The Isothrum checkout this xtask was built from (its repository root).
pub(crate) fn root() -> PathBuf {
    let xtask = Path::new(env!("CARGO_MANIFEST_DIR"));
    xtask.parent().map_or_else(|| xtask.to_path_buf(), Path::to_path_buf)
}

/// The build folder: `CARGO_TARGET_DIR`, else the workspace's `target`.
pub(crate) fn target_dir(root: &Path) -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from)
}

/// The workspace at `root`, as `cargo metadata --no-deps` describes it.
///
/// # Errors
///
/// `cargo metadata` fails, or prints no JSON.
pub(crate) fn metadata(root: &Path) -> crate::Res<serde_json::Value> {
    let mut cmd = crate::process::cargo(root);
    cmd.args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
        .arg(root.join("Cargo.toml"));
    Ok(serde_json::from_str(&crate::process::output(&mut cmd)?)?)
}

/// A path as text with `/` separators, as the tools on every host read it.
pub(crate) fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// `root/dir`, `dir` written with `/`.
pub(crate) fn path_in(root: &Path, dir: &str) -> PathBuf {
    dir.split('/').fold(root.to_path_buf(), |p, part| p.join(part))
}

/// Removes a file, or a folder with what it holds, when it is there.
pub(crate) fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Copies folder `from` into `to`, recursively (a Rust copy: `cp -R` isn't on Windows).
pub(crate) fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_written_with_slashes_is_joined_part_by_part() {
        assert_eq!(
            path_in(Path::new("/ws"), "crates/isothrum-runner"),
            Path::new("/ws").join("crates").join("isothrum-runner")
        );
        assert_eq!(slashed(Path::new("crates/x")), "crates/x");
    }

    #[test]
    fn a_copied_folder_holds_what_the_original_did_and_goes_when_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        std::fs::create_dir_all(from.join("inner")).unwrap();
        std::fs::write(from.join("inner/file.txt"), "x").unwrap();
        copy_dir(&from, &to).unwrap();
        assert_eq!(std::fs::read_to_string(to.join("inner/file.txt")).unwrap(), "x");
        remove(&to).unwrap();
        assert!(!to.exists());
        remove(&to).unwrap();
    }
}
