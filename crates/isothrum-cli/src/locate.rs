//! Where things are: the run manifest of a run folder, and the runner's home.

use std::path::{Path, PathBuf};

use isoloom_manifest::contract::OUTPUT_DIR;
use isothrum_runner::{MANIFEST_FILE, is_outputs_folder};

use crate::error::{Error, Result};

/// The manifest to run: `run` itself when it is a file; else one Isoloom generated in an outputs
/// folder of `run` (`.isoloom/`, or an instance's `.isoloom-<n>/`: [`is_outputs_folder`]) for
/// the target asked for, or the only one generated. A target is a folder of an outputs folder
/// (`docker`) or, for a target that writes one manifest per cloud, one under it (`cloud-vm/aws`);
/// `--target cloud-vm` names the only cloud generated. A manifest of an instance is named with its
/// folder (`.isoloom-2/docker`), and its target alone (`docker`) names it too; `.isoloom/<target>`
/// is taken first when it is there.
pub(crate) fn manifest(run: &Path, target: Option<&str>) -> Result<PathBuf> {
    if run.is_file() {
        return Ok(run.to_path_buf());
    }
    let outputs = run.join(OUTPUT_DIR);
    if let Some(path) = target.map(|t| outputs.join(t).join(MANIFEST_FILE)).filter(|p| p.is_file()) {
        return Ok(path);
    }
    let found = generated(run)?;
    let found: Vec<&Generated> = found.iter().filter(|g| target.is_none_or(|t| g.is_under(t))).collect();
    match (found.as_slice(), target) {
        ([one], _) => Ok(one.path.clone()),
        ([], Some(target)) => Err(Error::Usage(format!(
            "{} doesn't exist: run `isoloom generate --target {target}` first",
            outputs.join(target).join(MANIFEST_FILE).display()
        ))),
        ([], None) => Err(Error::Usage(format!(
            "no run manifest under {} (nor an instance's {OUTPUT_DIR}-<n>): run `isoloom generate --target <target>` first",
            outputs.display()
        ))),
        (many, _) => {
            let names: Vec<String> = many.iter().map(|g| g.name()).collect();
            Err(Error::Usage(format!(
                "{} has manifests for {}: choose one with --target",
                run.display(),
                names.join(", ")
            )))
        }
    }
}

/// A run manifest Isoloom generated.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Generated {
    /// Its outputs folder, when it is an instance's (`.isoloom-2`); `None` for `.isoloom`.
    instance: Option<String>,
    /// Its target, a folder of the outputs folder, and its cloud's folder under it, if any
    /// (`cloud-vm/aws`).
    target: String,
    /// Its file.
    path: PathBuf,
}

impl Generated {
    /// What it is called: its target, after its instance's folder (`.isoloom-2/docker`).
    fn name(&self) -> String {
        self.instance
            .as_ref()
            .map_or_else(|| self.target.clone(), |folder| format!("{folder}/{}", self.target))
    }

    /// Whether `target` names it: its name or its target, whole, or a cloud of either.
    fn is_under(&self, target: &str) -> bool {
        is_under(&self.name(), target) || is_under(&self.target, target)
    }
}

/// Whether the target `name` (`cloud-vm/aws`) is `target` or one of its clouds.
fn is_under(name: &str, target: &str) -> bool {
    name == target || name.strip_prefix(target).is_some_and(|rest| rest.starts_with('/'))
}

/// Every manifest generated in `run`'s outputs folders, sorted (`.isoloom/`'s first): each
/// `<target>/run.json`, else each `<target>/<cloud>/run.json`. Only a missing folder means nothing
/// was generated; any other failure to list a folder (an outputs folder's name on a file among
/// them) is said as it is.
fn generated(run: &Path) -> Result<Vec<Generated>> {
    let mut found = Vec::new();
    for (name, outputs) in entries(run)?.into_iter().filter(|(name, _)| is_outputs_folder(name)) {
        let instance = (name != OUTPUT_DIR).then_some(name);
        for (target, folder) in folders(&outputs)? {
            let path = folder.join(MANIFEST_FILE);
            if path.is_file() {
                found.push(Generated {
                    instance: instance.clone(),
                    target,
                    path,
                });
                continue;
            }
            for (cloud, sub) in folders(&folder)? {
                let cloud_manifest = sub.join(MANIFEST_FILE);
                if cloud_manifest.is_file() {
                    found.push(Generated {
                        instance: instance.clone(),
                        target: format!("{target}/{cloud}"),
                        path: cloud_manifest,
                    });
                }
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The folders in `dir`, by name (none when `dir` doesn't exist).
fn folders(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    Ok(entries(dir)?.into_iter().filter(|(_, path)| path.is_dir()).collect())
}

/// Everything in `dir`, by name (nothing when `dir` doesn't exist).
fn entries(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let unread = |source| Error::Read {
        path: dir.to_path_buf(),
        source,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(unread(source)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(unread)?.path();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        found.push((name, path));
    }
    Ok(found)
}

/// The runner's home: `--home`, else `$ISOTHRUM_HOME`, else `~/.isothrum`.
pub(crate) fn home(flag: Option<PathBuf>, env: Option<std::ffi::OsString>, user_home: Option<PathBuf>) -> Result<PathBuf> {
    flag.or_else(|| env.filter(|h| !h.is_empty()).map(PathBuf::from))
        .or_else(|| user_home.map(|h| h.join(".isothrum")))
        .ok_or_else(|| Error::Usage("no home folder: give --home or set ISOTHRUM_HOME".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_only_generated_target_is_found_else_one_must_be_named() {
        let run = tempfile::tempdir().unwrap();
        assert!(manifest(run.path(), None).unwrap_err().to_string().contains("isoloom generate"));
        let docker = run.path().join(".isoloom/docker/run.json");
        std::fs::create_dir_all(docker.parent().unwrap()).unwrap();
        std::fs::write(&docker, "{}").unwrap();
        assert_eq!(manifest(run.path(), None).unwrap(), docker);
        assert_eq!(manifest(&docker, None).unwrap(), docker);
        std::fs::create_dir_all(run.path().join(".isoloom/vagrant")).unwrap();
        std::fs::write(run.path().join(".isoloom/vagrant/run.json"), "{}").unwrap();
        assert!(
            manifest(run.path(), None)
                .unwrap_err()
                .to_string()
                .contains("docker, vagrant: choose one with --target")
        );
        assert_eq!(manifest(run.path(), Some("docker")).unwrap(), docker);
        assert!(manifest(run.path(), Some("proxmox")).is_err());
    }

    /// A target that writes one manifest per cloud (`cloud-vm/aws`) is found by its cloud, by
    /// the target when it has one cloud, or alone.
    #[test]
    fn a_cloud_manifest_is_found_one_folder_down() {
        let run = tempfile::tempdir().unwrap();
        let aws = run.path().join(".isoloom/cloud-vm/aws/run.json");
        std::fs::create_dir_all(aws.parent().unwrap()).unwrap();
        std::fs::write(&aws, "{}").unwrap();
        assert_eq!(manifest(run.path(), None).unwrap(), aws);
        assert_eq!(manifest(run.path(), Some("cloud-vm")).unwrap(), aws);
        assert_eq!(manifest(run.path(), Some("cloud-vm/aws")).unwrap(), aws);
        assert!(manifest(run.path(), Some("cloud")).is_err(), "a target is a whole folder name");

        let gcp = run.path().join(".isoloom/cloud-vm/gcp/run.json");
        std::fs::create_dir_all(gcp.parent().unwrap()).unwrap();
        std::fs::write(&gcp, "{}").unwrap();
        let several = manifest(run.path(), Some("cloud-vm")).unwrap_err().to_string();
        assert!(several.contains("cloud-vm/aws, cloud-vm/gcp: choose one with --target"), "{several}");
        assert_eq!(manifest(run.path(), Some("cloud-vm/gcp")).unwrap(), gcp);
    }

    /// Regression: only `.isoloom/` was read, so a project generated as an instance
    /// (`isoloom generate --instance 2`, written to `.isoloom-2/`) had no manifest. An instance's
    /// is found too, named with its folder, by its target alone or with the folder; `.isoloom`'s
    /// own target comes first; a folder that only starts like one (`.isoloom-reach`) isn't read.
    #[test]
    fn an_instances_manifest_is_found_in_its_own_outputs_folder() {
        let run = tempfile::tempdir().unwrap();
        let write = |path: &str| {
            let path = run.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}").unwrap();
            path
        };
        write(".isoloom-reach/docker/run.json");
        write(".isoloomfoo/docker/run.json");
        let second = write(".isoloom-2/docker/run.json");
        assert_eq!(manifest(run.path(), None).unwrap(), second);
        assert_eq!(manifest(run.path(), Some("docker")).unwrap(), second);
        let aws = write(".isoloom-3/cloud-vm/aws/run.json");
        let both = manifest(run.path(), None).unwrap_err().to_string();
        assert!(both.contains(".isoloom-2/docker, .isoloom-3/cloud-vm/aws: choose one with --target"), "{both}");
        assert_eq!(manifest(run.path(), Some("cloud-vm")).unwrap(), aws);
        assert_eq!(manifest(run.path(), Some(".isoloom-3/cloud-vm/aws")).unwrap(), aws);
        let own = write(".isoloom/docker/run.json");
        assert_eq!(manifest(run.path(), Some("docker")).unwrap(), own, "the project's own first");
        assert_eq!(manifest(run.path(), Some(".isoloom-2/docker")).unwrap(), second);
        let all = manifest(run.path(), None).unwrap_err().to_string();
        assert!(all.contains("docker, .isoloom-2/docker, .isoloom-3/cloud-vm/aws: choose"), "{all}");
    }

    /// A `.isoloom` that exists but can't be listed (here a file in its place) is said as it is,
    /// not as nothing generated.
    #[test]
    fn an_unreadable_outputs_folder_is_no_missing_manifest() {
        let run = tempfile::tempdir().unwrap();
        std::fs::write(run.path().join(".isoloom"), "not a folder").unwrap();
        let err = manifest(run.path(), None).unwrap_err();
        assert!(matches!(err, Error::Read { .. }), "{err}");
        assert!(!err.to_string().contains("isoloom generate"), "{err}");
    }

    #[test]
    fn the_home_is_the_flag_else_the_variable_else_the_users() {
        assert_eq!(home(Some("/a".into()), Some("/b".into()), Some("/u".into())).unwrap(), PathBuf::from("/a"));
        assert_eq!(home(None, Some("/b".into()), Some("/u".into())).unwrap(), PathBuf::from("/b"));
        assert_eq!(
            home(None, Some(std::ffi::OsString::new()), Some("/u".into())).unwrap(),
            PathBuf::from("/u/.isothrum")
        );
        assert!(home(None, None, None).is_err());
    }

    /// Regression: an `ISOTHRUM_HOME` that isn't UTF-8 was read as unset, and the runs went to
    /// `~/.isothrum`; it is a path like `--home`'s.
    #[cfg(unix)]
    #[test]
    fn a_home_variable_that_isnt_utf8_is_still_the_home() {
        use std::os::unix::ffi::OsStringExt;
        let raw = std::ffi::OsString::from_vec(b"/runs/bad\xff".to_vec());
        assert_eq!(home(None, Some(raw.clone()), Some("/u".into())).unwrap(), PathBuf::from(raw));
    }
}
