//! An owner-only file a secret waits in while one tool reads it: a guest delivery's input, a
//! Proxmox console request's `curl` config. It goes when the tool is done, however that ends.

use std::path::{Path, PathBuf};

use crate::Result;

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
