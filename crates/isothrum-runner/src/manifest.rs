//! Reading a run manifest from its file: the contract's own parser (`isoloom-manifest`, which
//! checks the version, the types and what spans fields), plus where it lives.

use std::path::{Path, PathBuf};

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
