//! Owner-only files and folders, written whole. A run's state holds the pinned ports, its record and, on other
//! targets, a Terraform state with sensitive outputs: on Unix files are 0600 and folders 0700
//! (the usual umask would leave them readable by every local account). Windows' per-user profile
//! already keeps other accounts out.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Creates `dir` (with its missing parents) and makes it owner-only.
pub(crate) fn private_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(dir)?.permissions().mode() & 0o777 != DIR_MODE {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(DIR_MODE))?;
        }
    }
    Ok(())
}

/// Writes `contents` to `path` whole and owner-only: to a temporary file beside it, flushed, then
/// renamed over it, so a crash or a full disk mid-write leaves the previous file intact.
pub(crate) fn write_private(path: &Path, contents: &[u8]) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    // Unique per write, so two writers never share a temporary file.
    let tmp = path.with_file_name(format!(".{name}.{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let written = (|| {
        let mut f = open(&tmp, std::fs::OpenOptions::new().write(true).create_new(true))?;
        f.write_all(contents)?;
        f.sync_all()
    })();
    let result = written.and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err()
        && let Err(e) = std::fs::remove_file(&tmp)
    {
        // The write's own error is the one that matters; the leftover is a dot-file beside it.
        tracing::debug!(error = %e, path = %tmp.display(), "a temporary file was left behind");
    }
    result
}

/// Opens `path` to append to it, owner-only: created when missing.
pub(crate) fn append_private(path: &Path) -> io::Result<File> {
    open(path, std::fs::OpenOptions::new().append(true).create(true))
}

/// Creates `path` (emptying it when it exists) for writing, owner-only: a file written as a
/// stream, then renamed into place by its writer.
pub(crate) fn create_private(path: &Path) -> io::Result<File> {
    open(path, std::fs::OpenOptions::new().write(true).create(true).truncate(true))
}

/// Opens (creating when missing) `path` for writing without truncating it, owner-only: a lock file.
pub(crate) fn lock_file(path: &Path) -> io::Result<File> {
    open(path, std::fs::OpenOptions::new().write(true).create(true).truncate(false))
}

fn open(path: &Path, options: &mut std::fs::OpenOptions) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        options.mode(FILE_MODE);
        let f = options.open(path)?;
        // `mode` only applies to a file it creates: narrow an older, wider one.
        if f.metadata()?.permissions().mode() & 0o777 != FILE_MODE {
            f.set_permissions(std::fs::Permissions::from_mode(FILE_MODE))?;
        }
        Ok(f)
    }
    #[cfg(not(unix))]
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_replaced_whole_and_no_temporary_file_is_left() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("record.json");
        write_private(&f, b"{\"a\":1}").unwrap();
        write_private(&f, b"{}").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{}");
        let names: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names, ["record.json"]);
        // A target that can't be replaced (a folder) fails and leaves nothing behind.
        std::fs::create_dir_all(dir.path().join("folder.json").join("x")).unwrap();
        assert!(write_private(&dir.path().join("folder.json"), b"{}").is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn files_and_folders_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        private_dir(&runs).unwrap();
        write_private(&runs.join("r.json"), b"x").unwrap();
        let wide = runs.join("events.jsonl");
        std::fs::write(&wide, "").unwrap();
        std::fs::set_permissions(&wide, std::fs::Permissions::from_mode(0o644)).unwrap();
        drop(append_private(&wide).unwrap());
        assert_eq!((mode(&runs), mode(&runs.join("r.json")), mode(&wide)), (0o700, 0o600, 0o600));
    }
}
