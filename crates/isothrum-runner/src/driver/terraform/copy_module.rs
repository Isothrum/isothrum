//! `terraform.copy_module`: the run's own Terraform module (cloud-services: the project's
//! `terraform/`) copied into `${run.state_dir}/module`, where the run's steps apply it, so its
//! lock file and whatever it writes stay with the run, out of the project.
//!
//! The copy's Terraform files (`*.tf`, `*.tf.json`, `*.tfvars`, `*.tfvars.json`) are removed
//! first, so a file renamed or removed in the module isn't applied any more; everything else the
//! module wrote there is kept. `.terraform` folders and state files (`*.tfstate*`) are never
//! copied. With `when: missing` it copies only when there is no copy yet: a `down` destroys with
//! the module as it was applied. The module must be inside the run's project.

use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::executor::Cx;
use crate::store::if_there;

/// The copy, in the run's state folder.
pub(super) const COPY: &str = "module";

/// What a module copy replaces: Terraform's own files.
const TERRAFORM_FILES: [&str; 4] = [".tf", ".tf.json", ".tfvars", ".tfvars.json"];

/// The copy the action of the step at `at` (`lifecycle.up.copy`) asks for: module `from` into
/// the run's state folder, only when there is none with `when` `missing`.
pub(super) fn run(cx: &Cx<'_>, at: &str, from: Option<&str>, when: Option<&str>) -> crate::Result<()> {
    let malformed = |field: &str, reason: String| Error::Malformed {
        at: format!("{at}.with.{field}"),
        reason,
    };
    let from = from.ok_or_else(|| malformed("from", "a `terraform.copy_module` needs the module to copy".into()))?;
    let only_missing = match when {
        None => false,
        Some("missing") => true,
        Some(other) => return Err(malformed("when", format!("`{other}`: only `missing` is a `when`"))),
    };
    let source = inside_project(cx.run.dir(), at, from)?;
    let copy = cx.state_dir().join(COPY);
    if only_missing && copy.is_dir() {
        return Ok(());
    }
    crate::store::create_dir(&copy)?;
    forget_terraform_files(&copy)?;
    copy_tree(&source, &copy)
}

/// `from` (relative to the manifest's folder `dir`) as a folder inside the run's project: the
/// folder above the `.isoloom…` one. The `with.from` of the step at `at` is malformed when it is
/// missing, outside the project or not a folder; a path that can't be resolved for another reason
/// is an I/O error.
pub(super) fn inside_project(dir: &Path, at: &str, from: &str) -> crate::Result<PathBuf> {
    let outside = || Error::Malformed {
        at: format!("{at}.with.from"),
        reason: format!("`{from}` isn't a folder inside the run's project"),
    };
    let resolve = |path: PathBuf| if_there(std::fs::canonicalize(&path)).map_err(Error::io(path, "resolved"));
    let project = resolve(crate::container::run_root(dir).to_path_buf())?.ok_or_else(outside)?;
    let source = resolve(dir.join(from))?.ok_or_else(outside)?;
    if source.starts_with(&project) && source.is_dir() {
        Ok(source)
    } else {
        Err(outside())
    }
}

/// Whether `dir` holds a Terraform file; a folder that isn't there holds none.
pub(super) fn has_terraform_files(dir: &Path) -> crate::Result<bool> {
    let Some(entries) = if_there(std::fs::read_dir(dir)).map_err(Error::io(dir, "read"))? else {
        return Ok(false);
    };
    for entry in entries {
        if is_terraform_file(&entry.map_err(Error::io(dir, "read"))?.path()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_terraform_file(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.is_file() && TERRAFORM_FILES.iter().any(|ext| name.ends_with(ext))
}

/// Removes the Terraform files under `dir` (its `.terraform` left alone).
fn forget_terraform_files(dir: &Path) -> crate::Result<()> {
    let entries = std::fs::read_dir(dir).map_err(Error::io(dir, "read"))?;
    for entry in entries {
        let entry = entry.map_err(Error::io(dir, "read"))?;
        let path = entry.path();
        if path.is_dir() && entry.file_name() != ".terraform" {
            forget_terraform_files(&path)?;
        } else if is_terraform_file(&path) {
            std::fs::remove_file(&path).map_err(Error::io(&path, "removed"))?;
        }
    }
    Ok(())
}

/// Whether `name` stays out of a copy: Terraform's data folder, a state.
fn left_out(name: &str) -> bool {
    name == ".terraform" || name.contains(".tfstate")
}

/// Copies `from` into `to`, recursively, but what [`left_out`] names.
fn copy_tree(from: &Path, to: &Path) -> crate::Result<()> {
    let entries = std::fs::read_dir(from).map_err(Error::io(from, "read"))?;
    for entry in entries {
        let entry = entry.map_err(Error::io(from, "read"))?;
        let name = entry.file_name();
        if left_out(&name.to_string_lossy()) {
            continue;
        }
        let (source, target) = (entry.path(), to.join(&name));
        if source.is_dir() {
            crate::store::create_dir(&target)?;
            copy_tree(&source, &target)?;
        } else {
            std::fs::copy(&source, &target).map_err(Error::io(&target, "written"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
