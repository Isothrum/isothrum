//! Taking over a deployment applied outside the runner: its local state moved into the run's.

use super::{BACKUP_FILE, Error, Path, Result, STATE_FILE, Terraform, ToolCx};

impl Terraform {
    /// Takes over the deployment whose local state is in `from` (applied outside the runner from
    /// the run's module): its state, and its backup when there is one, move into the run's state
    /// folder, which later operations use as the run's own.
    ///
    /// # Errors
    ///
    /// `adopt` when the run has a state already or `from` holds none; `io` when a file can't be
    /// moved (what moved is put back). The files moved, for [`Terraform::give_back`].
    pub(crate) fn adopt(cx: &ToolCx<'_>, from: &Path) -> Result<Vec<&'static str>> {
        let refuse = |reason: String| Error::Adopt {
            run: cx.run.to_string(),
            reason,
        };
        if cx.state_dir.join(STATE_FILE).exists() {
            return Err(refuse("the run has a Terraform state already".into()));
        }
        if !from.join(STATE_FILE).is_file() {
            return Err(refuse(format!("{} holds no {STATE_FILE}", from.display())));
        }
        let mut moved = Vec::new();
        for file in [STATE_FILE, BACKUP_FILE] {
            let (src, dst) = (from.join(file), cx.state_dir.join(file));
            if !src.exists() {
                continue;
            }
            if let Err(e) = move_file(&src, &dst) {
                Terraform::give_back(cx, from, &moved);
                return Err(Error::io(&src, "moved")(e));
            }
            moved.push(file);
        }
        Ok(moved)
    }

    /// Puts `files` an [`Terraform::adopt`] moved from `from` back (an adoption whose record
    /// couldn't be written), best effort.
    pub(crate) fn give_back(cx: &ToolCx<'_>, from: &Path, files: &[&str]) {
        for file in files {
            if let Err(e) = move_file(&cx.state_dir.join(file), &from.join(file)) {
                tracing::warn!(file, error = %e, "an adopted state file couldn't be put back");
            }
        }
    }
}

/// Moves `from` to `to`: a rename, else, only when they are on different file systems, a copy
/// then a removal. Any other failure to rename is the error.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
            std::fs::copy(from, to)?;
            // Never in both places: when `from` can't be removed, the copy goes too.
            std::fs::remove_file(from).inspect_err(|_| {
                if let Err(gone) = std::fs::remove_file(to) {
                    tracing::warn!(error = %gone, file = %to.display(), "a copy of a taken-over state couldn't be removed");
                }
            })
        }
        renamed => renamed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: every failed rename fell back to a copy, so a state whose folder can't be
    /// written was copied into the run and stayed in the old folder too. Only a move across file
    /// systems copies.
    #[cfg(unix)]
    #[test]
    fn a_state_that_cant_be_moved_stays_only_where_it_was() {
        use std::os::unix::fs::PermissionsExt;
        let (old, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (from, to) = (old.path().join("terraform.tfstate"), run.path().join("terraform.tfstate"));
        std::fs::write(&from, "{}").unwrap();
        std::fs::set_permissions(old.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let moved = move_file(&from, &to);
        std::fs::set_permissions(old.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        if moved.is_ok() {
            return; // root writes any folder: nothing to refuse
        }
        assert_eq!(moved.unwrap_err().kind(), std::io::ErrorKind::PermissionDenied);
        assert!(!to.exists(), "no second copy in the run");
        assert!(from.is_file(), "the state stays where it was");
    }
}
