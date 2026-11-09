//! Where things are: the run manifest of a run folder, and the runner's home.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// The manifest to run: `run` itself when it is a file; else `run/.isoloom/<target>/run.json`,
/// the target being the one asked for, or the only one generated.
pub(crate) fn manifest(run: &Path, target: Option<&str>) -> Result<PathBuf> {
    if run.is_file() {
        return Ok(run.to_path_buf());
    }
    let outputs = run.join(".isoloom");
    if let Some(target) = target {
        let path = outputs.join(target).join("run.json");
        return if path.is_file() {
            Ok(path)
        } else {
            Err(Error::Usage(format!(
                "{} doesn't exist: run `isoloom generate --target {target}` first",
                path.display()
            )))
        };
    }
    // Only a missing folder means nothing was generated; any other failure is said as it is.
    let mut found: Vec<PathBuf> = match std::fs::read_dir(&outputs) {
        Ok(entries) => entries.flatten().map(|e| e.path().join("run.json")).filter(|p| p.is_file()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(Error::Read { path: outputs, source }),
    };
    found.sort();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => Err(Error::Usage(format!(
            "no run manifest under {}: run `isoloom generate --target <target>` first",
            outputs.display()
        ))),
        many => {
            let names: Vec<String> = many
                .iter()
                .filter_map(|p| Some(p.parent()?.file_name()?.to_string_lossy().into_owned()))
                .collect();
            Err(Error::Usage(format!(
                "{} has manifests for {}: choose one with --target",
                run.display(),
                names.join(", ")
            )))
        }
    }
}

/// The runner's home: `--home`, else `$ISOTHRUM_HOME`, else `~/.isothrum`.
pub(crate) fn home(flag: Option<PathBuf>, env: Option<String>, user_home: Option<PathBuf>) -> Result<PathBuf> {
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
        assert_eq!(home(None, Some(String::new()), Some("/u".into())).unwrap(), PathBuf::from("/u/.isothrum"));
        assert!(home(None, None, None).is_err());
    }
}
