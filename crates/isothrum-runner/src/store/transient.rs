//! An owner-only file a secret waits in while one tool reads it: a guest delivery's input, a
//! Proxmox console request's `curl` config. It goes when the tool is done, however that ends.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::error::Error;

/// An owner-only file in the run's state folder, removed when its use ends: by
/// [`Transient::remove_after`] once the tool ran (its removal failing is an error), and on drop
/// when the operation is cut short meanwhile (its future dropped: an embedder's cancel, Ctrl-C, a
/// panic unwinding).
pub(crate) struct Transient {
    /// Where it is.
    path: PathBuf,
    /// What it is, as a trace names it when its removal on drop fails.
    what: &'static str,
    /// Whether it is still to be removed.
    held: bool,
}

impl Transient {
    /// `contents` written owner-only at `path`, `what` naming it.
    pub(crate) fn write(path: PathBuf, contents: &[u8], what: &'static str) -> Result<Transient> {
        super::write(&path, contents)?;
        Ok(Transient { path, what, held: true })
    }

    /// `contents` written owner-only in a new file of its own in `dir`, named
    /// `<stem>-<random>.<extension>`, `what` naming it: two uses at once (requests that take no
    /// lock) never read, replace or remove each other's.
    pub(crate) fn write_unique(dir: &Path, (stem, extension): (&str, &str), contents: &[u8], what: &'static str) -> Result<Transient> {
        let mut tries = 0;
        loop {
            let path = dir.join(format!("{stem}-{}.{extension}", suffix()?));
            match super::write_new_private(&path, contents) {
                Ok(()) => return Ok(Transient { path, what, held: true }),
                // A name already taken (as good as never with 64 random bits): another one.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
                Err(e) => return Err(Error::io(&path, "written")(e)),
            }
        }
    }

    /// Where it is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Removes it once `done` (what its tool gave): a file that can't be removed fails what
    /// succeeded, naming it; when `done` failed already that error stands and the removal's is
    /// traced.
    pub(crate) fn remove_after<T>(mut self, done: Result<T>) -> Result<T> {
        self.held = false;
        let removed = super::remove(&self.path);
        if let (Err(_), Err(e)) = (&done, &removed) {
            tracing::error!(error = %e, "{} was left in the run's state folder", self.what);
        }
        done.and_then(|value| removed.map(|()| value))
    }
}

/// 64 random bits, as lowercase hex.
fn suffix() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|e| Error::io("the OS's random source", "read")(std::io::Error::other(e.to_string())))?;
    Ok(bytes.iter().fold(String::with_capacity(16), |mut hex, b| {
        hex.extend(char::from_digit(u32::from(b >> 4), 16));
        hex.extend(char::from_digit(u32::from(b & 15), 16));
        hex
    }))
}

impl Drop for Transient {
    fn drop(&mut self) {
        if self.held
            && let Err(e) = super::remove_path(&self.path)
        {
            tracing::error!(error = %e, path = %self.path.display(), "{}, cut short, was left in the run's state folder", self.what);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_goes_once_used_and_when_dropped_unused() {
        let dir = tempfile::tempdir().unwrap();
        let used = Transient::write(dir.path().join("a"), b"s3cret", "a test's file").unwrap();
        let used_at = used.path().to_path_buf();
        assert_eq!(used.remove_after(Ok(7)).unwrap(), 7);
        assert!(!used_at.exists());
        let cut = Transient::write(dir.path().join("b"), b"s3cret", "a test's file").unwrap();
        let cut_at = cut.path().to_path_buf();
        drop(cut);
        assert!(!cut_at.exists(), "a use cut short removes it too");
    }

    /// Regression: every Proxmox API request wrote the same file, so a status read beside a
    /// console request read, replaced or removed the other's credentials.
    #[test]
    fn each_unique_use_has_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let a = Transient::write_unique(dir.path(), ("req", "curl"), b"a", "a test's file").unwrap();
        let b = Transient::write_unique(dir.path(), ("req", "curl"), b"b", "a test's file").unwrap();
        assert_ne!(a.path(), b.path());
        let name = a.path().file_name().unwrap().to_str().unwrap().to_owned();
        let (stem, extension) = name.rsplit_once('.').unwrap();
        assert!(stem.starts_with("req-") && extension == "curl", "{name}");
        let b_at = b.path().to_path_buf();
        a.remove_after(Ok(())).unwrap();
        assert_eq!(std::fs::read(&b_at).unwrap(), b"b", "the other use's file is untouched");
        drop(b);
        assert!(!b_at.exists());
    }

    #[test]
    fn a_failed_use_keeps_its_own_error() {
        let dir = tempfile::tempdir().unwrap();
        let file = Transient::write(dir.path().join("a"), b"s3cret", "a test's file").unwrap();
        let at = file.path().to_path_buf();
        let failed: Result<()> = Err(crate::error::Error::NoConsole {
            machine: "web".into(),
            why: "refused".into(),
        });
        assert_eq!(file.remove_after(failed).unwrap_err().code(), crate::ErrorCode::NoConsole);
        assert!(!at.exists());
    }
}
