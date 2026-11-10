//! Owner-only files and folders, written whole. A run's state holds the pinned ports, its record and, on other
//! targets, a Terraform state with sensitive outputs: on Unix files are 0600 and folders 0700
//! (the usual umask would leave them readable by every local account). On Windows the home's
//! DACL keeps other accounts out of all of it ([`private_home`]).

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// Creates the runner's home (with its missing parents) and makes it owner-only with a DACL
/// everything under it inherits (`acl`), unless it is inside the user's `profile`.
#[cfg(windows)]
pub(crate) fn private_home(home: &Path, profile: Option<&Path>) -> io::Result<()> {
    std::fs::create_dir_all(home)?;
    super::acl::owner_only(home, profile)
}

/// Creates the runner's home (with its missing parents). Each folder under it is made 0700 as it
/// is created ([`private_dir`]), so the home itself, which may be a folder the user picked, keeps
/// its mode; the user's profile, which only a Windows home looks at, has no part.
#[cfg(not(windows))]
pub(crate) fn private_home(home: &Path, _profile: Option<&Path>) -> io::Result<()> {
    std::fs::create_dir_all(home)
}

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

/// What a read gave, `None` when there was nothing to read (`NotFound`); any other failure is
/// the error.
pub(crate) fn if_there<T>(read: io::Result<T>) -> io::Result<Option<T>> {
    match read {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Removes `path`, a file or a folder with all it holds; nothing there is fine. A symbolic link
/// (a Windows junction too) is removed itself, never followed.
pub(crate) fn remove_path(path: &Path) -> io::Result<()> {
    let Some(meta) = if_there(std::fs::symlink_metadata(path))? else {
        return Ok(());
    };
    let done = if meta.file_type().is_symlink() {
        // A link to a folder is a folder entry on Windows: removed as one there.
        std::fs::remove_file(path).or_else(|_| std::fs::remove_dir(path))
    } else if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    if_there(done).map(drop)
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
    fn a_file_a_folder_or_nothing_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (file, folder) = (dir.path().join("f"), dir.path().join("d"));
        std::fs::write(&file, "x").unwrap();
        std::fs::create_dir_all(folder.join("inner")).unwrap();
        std::fs::write(folder.join("inner/g"), "y").unwrap();
        for p in [&file, &folder] {
            remove_path(p).unwrap();
            assert!(std::fs::symlink_metadata(p).is_err(), "{}", p.display());
        }
        remove_path(&dir.path().join("missing")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_removed_never_followed() {
        let (base, elsewhere) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::write(elsewhere.path().join("keep.txt"), "x").unwrap();
        let (to_folder, to_file) = (base.path().join("folder-link"), base.path().join("file-link"));
        std::os::unix::fs::symlink(elsewhere.path(), &to_folder).unwrap();
        std::os::unix::fs::symlink(elsewhere.path().join("keep.txt"), &to_file).unwrap();
        for link in [&to_folder, &to_file] {
            remove_path(link).unwrap();
            assert!(std::fs::symlink_metadata(link).is_err(), "{}", link.display());
        }
        assert!(elsewhere.path().join("keep.txt").is_file());
    }

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
