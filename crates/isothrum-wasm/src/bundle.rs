//! The zips the user downloads: the whole project, or one target's files.

use std::io::{Cursor, Write};

use zip::write::SimpleFileOptions;

use crate::plan::{compiled, planned};
use crate::{Error, File, Result};

/// Where the spec goes in a download.
const SPEC_PATH: &str = "isoloom.yml";

/// The zip the user downloads: `isoloom.yml` at the root, the project's own files where it points,
/// and each generated file at its path.
///
/// # Errors
///
/// [`Error`]: the topology is malformed or doesn't compile, Isoloom finds problems in the
/// spec, or the zip can't be written.
pub(crate) fn bundle_bytes(topology_json: &str) -> Result<Vec<u8>> {
    bundle_filtered(topology_json, None)
}

/// One target's zip: `isoloom.yml` and only that target's folder (`docker`, `hybrid`,
/// `cloud-docker/aws`...), with the project's own files that target's files use.
///
/// # Errors
///
/// [`Error`], as [`bundle_bytes`], or [`Error::NoFiles`] when the project generates
/// nothing for `target_dir`.
pub(crate) fn bundle_target_bytes(topology_json: &str, target_dir: &str) -> Result<Vec<u8>> {
    bundle_filtered(topology_json, Some(target_dir))
}

fn bundle_filtered(topology_json: &str, target_dir: Option<&str>) -> Result<Vec<u8>> {
    let (topo, compiled) = compiled(topology_json)?;
    let mut own: Vec<File> = compiled.files().iter().cloned().map(File::from).collect();
    let plan = planned(&topo, compiled);
    if !plan.problems.is_empty() {
        return Err(Error::Problems(plan.problems));
    }
    let files = target_files(plan.files, target_dir)?;
    // A one-target download only gets the project's own files its files use.
    own.retain(|o| target_dir.is_none() || uses(&files, &o.path));
    let spec = File {
        path: SPEC_PATH.into(),
        contents: plan.isoloom_yml.unwrap_or_default(),
    };
    zip_of(std::iter::once(&spec).chain(&own).chain(&files))
}

/// Whether `files` use the project's file at `path`: they name it by its path, or by its file
/// name (Isoloom's controller runs a playbook from its own folder). Naming it is enough: a file
/// kept that isn't run costs a few lines in the zip, one left out breaks the project.
fn uses(files: &[File], path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    files.iter().any(|f| f.contents.contains(path) || f.contents.contains(name))
}

/// The generated files of the target in `target_dir` (`docker`, `cloud-docker/aws`…), or all.
fn target_files(files: Vec<File>, target_dir: Option<&str>) -> Result<Vec<File>> {
    let prefix = target_dir.map(|d| format!(".isoloom/{}/", d.trim_matches('/')));
    let files: Vec<File> = files.into_iter().filter(|f| prefix.as_ref().is_none_or(|p| f.path.starts_with(p))).collect();
    if files.is_empty() {
        return Err(Error::NoFiles(target_dir.unwrap_or("any target").to_owned()));
    }
    Ok(files)
}

/// A zip of `files`, deflated, in order.
fn zip_of<'a>(files: impl Iterator<Item = &'a File>) -> Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for f in files {
        zip.start_file(f.path.as_str(), options)?;
        zip.write_all(f.contents.as_bytes())?;
    }
    Ok(zip.finish()?.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(contents: &str) -> File {
        File {
            path: ".isoloom/vagrant/Vagrantfile".into(),
            contents: contents.into(),
        }
    }

    #[test]
    fn a_project_file_is_used_when_named_by_its_path_or_its_name() {
        let files = [file("COPY provision/db.sh /tmp/"), file("ansible-playbook -i inventory.ini dc01.yml")];
        assert!(uses(&files, "provision/db.sh"));
        assert!(uses(&files, "provision/dc01.yml"));
        assert!(!uses(&files, "provision/web.sh"));
        assert!(!uses(&[], "provision/db.sh"));
    }
}
