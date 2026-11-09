//! The `project.archive` action: the project as a gzipped tar in the run's state folder
//! (`${run.state_dir}/project.tgz`), for a later step to pipe into a guest (`ssh.run`'s `stdin`).
//! Built here (`tar`, `flate2`), so no host tool is needed and every host makes the same archive.
//!
//! What goes in is what the generated files copy: Isoloom's outputs included, version control and
//! the tools' state left out at any depth (`.git`, `.vagrant`, `.terraform`), the archives of
//! earlier copies left out at the top (`.isoloom-project*`), and what `.isoloomignore` lists left
//! out, by the run manifest's own rules ([`isoloom_manifest::ignore`]). Paths are relative to the
//! project (no top folder) with `/`; a symbolic link goes in as a link, never followed. A project
//! can be large: the archive is written on Tokio's blocking pool, to a temporary file renamed over
//! the last one, owner-only (a project may hold a run's keys).

use std::collections::BTreeMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use isoloom_manifest::ignore::{FILE as IGNORE_FILE, Rules};
use isoloom_manifest::{SnakeName, Step, Template};

use crate::Result;
use crate::error::Error;
use crate::executor::Cx;

/// The archive's name in the run's state folder.
pub(crate) const ARCHIVE: &str = "project.tgz";

/// Never copied, at any depth: version control and the tools' own state.
const SKIP: [&str; 3] = [".git", ".vagrant", ".terraform"];

/// The prefix of an earlier copy's archive, left at the project's top by a generated module.
const EARLIER_ARCHIVE: &str = ".isoloom-project";

/// Runs `project.archive` for step `step`: the project at `with.root` (relative to the manifest's
/// folder) archived as the run's [`ARCHIVE`].
pub(crate) async fn project(cx: &Cx<'_>, step: &Step, with: &BTreeMap<SnakeName, Template>) -> Result<()> {
    let at = format!("{}.with.root", step.id);
    let root = match with.iter().find(|(k, _)| k.as_str() == "root") {
        Some((_, t)) => cx.vars.render(t, &at)?,
        None => {
            return Err(Error::Malformed {
                at,
                reason: "a `project.archive` needs the folder to archive".to_owned(),
            });
        }
    };
    let root = cx.run.dir().join(root);
    let out = cx.run.runner.store.run_dir(cx.run.name())?.join(ARCHIVE);
    let target = out.clone();
    let written = tokio::task::spawn_blocking(move || write(&root, &target)).await;
    match written {
        Ok(done) => done.map_err(Error::io(&out, "written")),
        Err(e) => Err(Error::io(&out, "written")(io::Error::other(e.to_string()))),
    }
}

/// Writes the archive of the project at `root` to `out`.
fn write(root: &Path, out: &Path) -> io::Result<()> {
    let rules = match std::fs::read_to_string(root.join(IGNORE_FILE)) {
        Ok(text) => Rules::parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Rules::default(),
        Err(e) => return Err(e),
    };
    let entries = entries(root, &rules)?;
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

/// What happens to `rel` (from the project's root, with `/`), a folder when `folder`.
fn visit(rel: &str, folder: bool, rules: &Rules) -> Visit {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    let top = !rel.contains('/');
    if SKIP.contains(&name) || (top && name.starts_with(EARLIER_ARCHIVE)) {
        return Visit { add: false, descend: false };
    }
    let ignored = if folder { rules.ignored(&format!("{rel}/")) } else { rules.ignored(rel) };
    Visit {
        add: !ignored,
        descend: folder && (!ignored || rules.brings_back()),
    }
}

/// Every path of the project at `root` that goes in, parents first, each folder's entries in
/// name order.
fn entries(root: &Path, rules: &Rules) -> io::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut pending: Vec<String> = vec![String::new()];
    while let Some(dir) = pending.pop() {
        let mut names: Vec<String> = std::fs::read_dir(root.join(&dir))?
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<_>>()?;
        names.sort();
        let mut folders = Vec::new();
        for name in names {
            let rel = if dir.is_empty() { name } else { format!("{dir}/{name}") };
            let folder = std::fs::symlink_metadata(root.join(&rel))?.is_dir();
            let v = visit(&rel, folder, rules);
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

/// Removes the run's archive once the run is down: nothing is left to copy it to.
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    let path: PathBuf = cx.run.runner.store.run_dir(cx.run.name())?.join(ARCHIVE);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(Error::io(&path, "removed")(e)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests;
