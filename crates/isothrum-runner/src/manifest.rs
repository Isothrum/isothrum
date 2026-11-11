//! Reading a run manifest from its file: the contract's own parser (`isoloom-manifest`, which
//! checks the version, the types and what spans fields), plus where it lives.

use std::path::{Path, PathBuf};

use isoloom_manifest::contract::OUTPUT_DIR;
use isoloom_manifest::{Manifest, Part, Placeholder, RunVar};

use crate::Result;
use crate::error::Error;

/// A manifest read from its file.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LoadedManifest {
    /// The manifest.
    pub manifest: Manifest,
    /// Its file, absolute.
    pub path: PathBuf,
    /// Its folder, absolute: `${run.dir}`, and the folder its relative paths start from.
    pub dir: PathBuf,
}

/// Reads and checks the manifest at `path`.
///
/// # Errors
///
/// Fails with `io` when the file can't be read, with `manifest_invalid` when it isn't UTF-8
/// (naming the byte), and with `manifest_invalid` or `manifest_version` when the contract refuses
/// it (the error names the field and the fix).
pub fn load_manifest(path: &Path) -> Result<LoadedManifest> {
    let path = std::path::absolute(path).map_err(Error::io(path, "read"))?;
    let bytes = std::fs::read(&path).map_err(Error::io(&path, "read"))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| Error::Malformed {
        at: path.display().to_string(),
        reason: format!("not UTF-8 at byte {}", e.valid_up_to()),
    })?;
    let manifest = isoloom_manifest::parse(text).map_err(|source| Error::Manifest { path: path.clone(), source })?;
    let dir = path.parent().map_or_else(|| path.clone(), Path::to_path_buf);
    Ok(LoadedManifest { manifest, path, dir })
}

/// The file Isoloom writes a target's run manifest to, in the target's folder of an outputs
/// folder ([`is_outputs_folder`]): `.isoloom/docker/run.json`, `.isoloom-2/cloud-vm/aws/run.json`.
pub const MANIFEST_FILE: &str = "run.json";

/// The highest instance number Isoloom writes a project's outputs for.
const MAX_INSTANCE: u8 = 99;

/// Whether a folder named `name`, at a project's top, is one Isoloom writes the project's outputs
/// under: [`OUTPUT_DIR`] (`.isoloom`), or an instance's `.isoloom-<n>`, `n` from 1 to 99 as
/// Isoloom writes it (no sign, no leading zero). Not `.isoloom-reach` nor any other `.isoloom*`
/// name. Isoloom publishes the way there (`isoloom_spec::instance::output_dir`) but not this way
/// back; a test holds this copy to the folders Isoloom generates.
#[must_use]
pub fn is_outputs_folder(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(OUTPUT_DIR) else {
        return false;
    };
    let Some(n) = rest.strip_prefix('-') else {
        return rest.is_empty();
    };
    !n.starts_with('0') && n.bytes().all(|b| b.is_ascii_digit()) && n.parse::<u8>().is_ok_and(|n| (1..=MAX_INSTANCE).contains(&n))
}

/// Whether a template of `manifest` names a `${run.<var>}` that `wanted` takes.
pub(crate) fn names_run_var(manifest: &Manifest, wanted: impl Fn(RunVar) -> bool) -> bool {
    manifest
        .templates()
        .iter()
        .any(|(_, t)| t.parts().iter().any(|p| matches!(p, Part::Placeholder(Placeholder::Run(v)) if wanted(*v))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule held to what Isoloom generates: the project's own outputs and every instance's
    /// (1 to 99, the numbers Isoloom takes) are in a folder it names, under the file it names;
    /// nothing else that starts the same is.
    #[test]
    fn an_outputs_folder_is_one_isoloom_generates_into() {
        let spec = "version: 1\nname: one\nnetworks:\n  app: { cidr: 10.80.0.0/24 }\nmachines:\n  web: { app: { image: nginx }, networks: { app: 10 } }\n";
        let lab = isoloom_targets::Lab::parse(spec).unwrap();
        let docker = lab.possible().into_iter().find(|t| t.id() == "docker").unwrap();
        let manifest_of = |files: Vec<isoloom_targets::GeneratedFile>| {
            files
                .into_iter()
                .map(|f| f.path)
                .find(|p| p.ends_with("/run.json") && p.contains("/docker/"))
                .unwrap()
        };
        let mut paths = vec![manifest_of(isoloom_targets::generate(&lab, docker).unwrap())];
        paths.extend((1..=MAX_INSTANCE).map(|n| manifest_of(isoloom_targets::generate_instance(&lab, docker, n).unwrap())));
        for path in &paths {
            let (folder, rest) = path.split_once('/').unwrap();
            assert!(is_outputs_folder(folder), "{path}");
            assert_eq!(rest, format!("docker/{MANIFEST_FILE}"), "{path}");
        }
        for n in [0, MAX_INSTANCE + 1] {
            assert!(isoloom_targets::generate_instance(&lab, docker, n).is_err(), "Isoloom makes no instance {n}");
        }
        for other in [
            ".isoloom-0",
            ".isoloom-100",
            ".isoloom-02",
            ".isoloom-+2",
            ".isoloom-",
            ".isoloom-reach",
            ".isoloomfoo",
            "isoloom",
            ".isoloom-2/",
        ] {
            assert!(!is_outputs_folder(other), "{other}");
        }
    }

    #[test]
    fn a_missing_file_is_an_io_error_naming_it() {
        let err = load_manifest(Path::new("no/such/run.json")).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::Io);
        assert!(err.to_string().contains("run.json couldn't be read"), "{err}");
    }

    /// Regression: bytes that weren't UTF-8 inside a JSON string read as U+FFFD, so a path or a
    /// template of the contract changed without a word.
    #[test]
    fn a_manifest_that_isnt_utf8_is_invalid_naming_the_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.json");
        let mut bytes = crate::testing::WEB_PORTAL.as_bytes().to_vec();
        let at = crate::testing::WEB_PORTAL.find("web-portal").unwrap();
        bytes[at] = 0xff;
        std::fs::write(&path, &bytes).unwrap();
        let err = load_manifest(&path).unwrap_err();
        assert_eq!(err.code(), crate::ErrorCode::ManifestInvalid, "{err}");
        assert!(err.to_string().ends_with(&format!("run.json: not UTF-8 at byte {at}")), "{err}");
    }
}
