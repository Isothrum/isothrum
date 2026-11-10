//! The `project.archive` action: the project as a gzipped tar in the run's state folder
//! (`${run.state_dir}/project.tgz`), for a later step to pipe into a guest (`ssh.run`'s `stdin`).
//! Built here (`tar`, `flate2`), so no host tool is needed and every host makes the same archive.
//!
//! `with.root` is the project's folder (relative to the manifest's) and `with.outputs`, optional,
//! the paths of Isoloom's outputs the guest reads (relative to the project, separated by spaces);
//! both are templates. What goes in is what the generated files copy, by the run manifest's own
//! rules ([`Rules::for_copy`]): what `.isoloomignore` lists left out, Isoloom's outputs left out
//! but `outputs`, and, at any depth (in those outputs too), version control and the tools' state
//! left out ([`never_copied`]: a Terraform state and its backups, which hold secrets in plain
//! text, a Vagrant machine's folder, the runner's logins). Those come last in the rules, so
//! nothing beneath a folder they match can come back: the walk never enters one. Paths are
//! relative to the project (no top folder) with `/`; a symbolic link goes in as a link, never
//! followed. A project can be large: the archive is written on Tokio's blocking pool, to a
//! temporary file renamed over the last one, owner-only (a project may hold a run's keys).

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use isoloom_manifest::ignore::{FILE as IGNORE_FILE, Rules, never_copied};
use isoloom_manifest::{SnakeName, Step, Template};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;

/// The archive's name in the run's state folder.
pub(crate) const ARCHIVE: &str = "project.tgz";

/// Runs `project.archive` for step `step`: the project at `with.root` (relative to the manifest's
/// folder) archived as the run's [`ARCHIVE`], with the outputs `with.outputs` names.
pub(crate) async fn project(cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let arg = |key: &str| cx.vars.with_arg(with, step.id.as_str(), key);
    let Some(root) = arg("root")? else {
        return Err(Error::Malformed {
            at: format!("{}.with.root", step.id),
            reason: "a `project.archive` needs the folder to archive".to_owned(),
        });
    };
    let outputs: Vec<String> = arg("outputs")?.unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    let root = cx.run.dir().join(root);
    let out = cx.run.runner.store.run_dir(cx.run.name())?.join(ARCHIVE);
    let target = out.clone();
    let written = tokio::task::spawn_blocking(move || write(&root, &outputs, &target)).await;
    match written {
        Ok(done) => done.map_err(Error::io(&out, "written")),
        Err(e) => Err(Error::io(&out, "written")(io::Error::other(e.to_string()))),
    }
}

/// Writes the archive of the project at `root` to `out`, with the Isoloom outputs `outputs`.
fn write(root: &Path, outputs: &[String], out: &Path) -> io::Result<()> {
    let ignore_text = crate::store::if_there(std::fs::read_to_string(root.join(IGNORE_FILE)))?.unwrap_or_default();
    let outputs: Vec<&str> = outputs.iter().map(String::as_str).collect();
    let entries = entries(root, &Filter::new(&ignore_text, &outputs))?;
    let part = out.with_file_name(format!("{ARCHIVE}.part"));
    let written = (|| {
        let file: File = crate::store::create_private(&part)?;
        let mut tar = tar::Builder::new(GzEncoder::new(file, Compression::default()));
        tar.follow_symlinks(false);
        for rel in &entries {
            tar.append_path_with_name(root.join(rel), rel)?;
        }
        tar.into_inner()?.finish()?.sync_all()
    })();
    match written.and_then(|()| std::fs::rename(&part, out)) {
        Ok(()) => Ok(()),
        Err(e) => {
            // The archive's own error is the one that matters.
            if let Err(left) = std::fs::remove_file(&part) {
                tracing::debug!(error = %left, path = %part.display(), "a partial archive was left behind");
            }
            Err(e)
        }
    }
}

/// What the walk does with one path of the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Visit {
    /// It goes in.
    add: bool,
    /// Its entries are walked (a folder that goes in, or one a `!` rule may bring something back
    /// from).
    descend: bool,
}

/// What the archive leaves out.
#[derive(Debug, Clone)]
struct Filter {
    /// The copy's rules: the project's `.isoloomignore`, then Isoloom's.
    copy: Rules,
    /// What no copy takes ([`never_copied`]): the last of `copy`, so nothing beneath a folder
    /// they match comes back.
    never: Rules,
}

impl Filter {
    /// The filter of a project whose `.isoloomignore` reads `ignore_text` (empty without one),
    /// with the Isoloom outputs `outputs`.
    fn new(ignore_text: &str, outputs: &[&str]) -> Filter {
        Filter {
            copy: Rules::for_copy(ignore_text, outputs),
            never: Rules::parse(&never_copied().collect::<Vec<_>>().join("\n")),
        }
    }

    /// What happens to `rel` (from the project's root, with `/`), a folder when `folder`.
    fn visit(&self, rel: &str, folder: bool) -> Visit {
        if folder && self.never.ignored(&format!("{rel}/")) {
            return Visit { add: false, descend: false };
        }
        let ignored = if folder {
            self.copy.ignored(&format!("{rel}/"))
        } else {
            self.copy.ignored(rel)
        };
        Visit {
            add: !ignored,
            descend: folder && (!ignored || self.copy.brings_back()),
        }
    }
}

/// Every path of the project at `root` that goes in, parents first, each folder's entries in
/// name order.
fn entries(root: &Path, filter: &Filter) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut pending: Vec<String> = vec![String::new()];
    while let Some(dir) = pending.pop() {
        let here = root.join(&dir);
        let mut names: Vec<String> = std::fs::read_dir(&here)?.map(|e| utf8_name(&here, e?.file_name())).collect::<io::Result<_>>()?;
        names.sort();
        let mut folders = Vec::new();
        for name in names {
            let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            let folder = std::fs::symlink_metadata(root.join(&rel))?.is_dir();
            let v = filter.visit(&rel, folder);
            if v.add {
                out.push(rel.clone());
            }
            if v.descend {
                folders.push(rel);
            }
        }
        // Depth first, in name order: the stack takes them last first.
        pending.extend(folders.into_iter().rev());
    }
    Ok(out)
}

/// The name of an entry of `folder`, refused when it isn't UTF-8: the ignore rules read names as
/// text, and a name made readable would name a path that isn't there.
fn utf8_name(folder: &Path, name: std::ffi::OsString) -> io::Result<String> {
    name.into_string().map_err(|name| {
        let path = folder.join(name);
        io::Error::new(io::ErrorKind::InvalidData, format!("{} has a name that isn't UTF-8", path.display()))
    })
}

/// Removes the run's archive once the run is down: nothing is left to copy it to.
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    let path: PathBuf = cx.run.runner.store.run_dir(cx.run.name())?.join(ARCHIVE);
    crate::store::if_there(std::fs::remove_file(&path))
        .map(drop)
        .map_err(Error::io(&path, "removed"))
}

#[cfg(test)]
mod tests;
