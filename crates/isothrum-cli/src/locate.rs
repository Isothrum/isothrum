//! Where things are: the run manifest of a run folder, and the runner's home.

use std::path::{Path, PathBuf};

use isoloom_manifest::contract::OUTPUT_DIR;

use crate::error::{Error, Result};

/// The manifest to run: `run` itself when it is a file; else the one Isoloom generated under
/// `run/.isoloom/` for the target asked for, or the only one generated. A target is a folder of
/// `.isoloom/` (`docker`) or, for a target that writes one manifest per cloud, one under it
/// (`cloud-vm/aws`), and `--target cloud-vm` names the only cloud generated.
pub(crate) fn manifest(run: &Path, target: Option<&str>) -> Result<PathBuf> {
    if run.is_file() {
        return Ok(run.to_path_buf());
    }
    let outputs = run.join(OUTPUT_DIR);
    if let Some(path) = target.map(|t| outputs.join(t).join(MANIFEST)).filter(|p| p.is_file()) {
        return Ok(path);
    }
    let found = generated(&outputs)?;
    let found: Vec<&(String, PathBuf)> = found.iter().filter(|(name, _)| target.is_none_or(|t| is_under(name, t))).collect();
    match (found.as_slice(), target) {
        ([(_, one)], _) => Ok(one.clone()),
        ([], Some(target)) => Err(Error::Usage(format!(
            "{} doesn't exist: run `isoloom generate --target {target}` first",
            outputs.join(target).join(MANIFEST).display()
        ))),
        ([], None) => Err(Error::Usage(format!(
            "no run manifest under {}: run `isoloom generate --target <target>` first",
            outputs.display()
        ))),
        (many, _) => {
            let names: Vec<&str> = many.iter().map(|(name, _)| name.as_str()).collect();
            Err(Error::Usage(format!(
                "{} has manifests for {}: choose one with --target",
                run.display(),
                names.join(", ")
            )))
        }
    }
}

/// The file Isoloom writes a target's manifest to.
const MANIFEST: &str = "run.json";

/// Whether the target `name` (`cloud-vm/aws`) is `target` or one of its clouds.
fn is_under(name: &str, target: &str) -> bool {
    name == target || name.strip_prefix(target).is_some_and(|rest| rest.starts_with('/'))
}

/// Every manifest generated under `outputs`, by target name, sorted: `<target>/run.json`, else
/// each `<target>/<cloud>/run.json`. Only a missing `outputs` means nothing was generated; any
/// other failure to list a folder is said as it is.
fn generated(outputs: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    for (target, folder) in folders(outputs)? {
        let path = folder.join(MANIFEST);
        if path.is_file() {
            found.push((target, path));
            continue;
        }
        for (cloud, sub) in folders(&folder)? {
            let cloud_manifest = sub.join(MANIFEST);
            if cloud_manifest.is_file() {
                found.push((format!("{target}/{cloud}"), cloud_manifest));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The folders in `dir`, by name (none when `dir` doesn't exist).
fn folders(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
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
        if path.is_dir() {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            found.push((name, path));
        }
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
