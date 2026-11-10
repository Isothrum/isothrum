//! The contract with Isoloom: every run-manifest fixture Isoloom ships (`isoloom-manifest`'s
//! `fixtures/`, found through Cargo wherever that crate lives) loads in the runner, and every
//! invalid one is refused with the code its `.error.json` names.

use std::error::Error as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use isothrum_runner::{Error, ErrorCode, load_manifest};
use serde_json::Value;

/// A test's outcome: a failed step is its report.
type Outcome<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `value[key]`, or null.
fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

/// `isoloom-manifest`'s fixtures folder: its manifest's folder as Cargo resolves it for this
/// crate (the crates.io release `Cargo.lock` locks, or the local Isoloom a `[patch.crates-io]`
/// points at). The workspace may hold another copy (another crate on another Isoloom version):
/// the runner's own is read.
fn fixtures() -> Outcome<PathBuf> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .output()?;
    if !out.status.success() {
        return Err(format!("cargo metadata: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    let meta: Value = serde_json::from_slice(&out.stdout)?;
    let packages = field(&meta, "packages").as_array().ok_or("no packages")?;
    let nodes = field(field(&meta, "resolve"), "nodes").as_array().ok_or("no resolve graph")?;
    let runner = nodes
        .iter()
        .find(|n| {
            packages
                .iter()
                .any(|p| field(p, "id") == field(n, "id") && field(p, "name") == env!("CARGO_PKG_NAME"))
        })
        .ok_or("this crate isn't in the graph")?;
    let dependencies = field(runner, "dependencies").as_array().ok_or("no dependencies")?;
    let package = packages
        .iter()
        .find(|p| field(p, "name") == "isoloom-manifest" && dependencies.contains(field(p, "id")))
        .ok_or("isoloom-manifest isn't among this crate's dependencies")?;
    let manifest = Path::new(field(package, "manifest_path").as_str().ok_or("no manifest path")?);
    Ok(manifest.parent().ok_or("a manifest at the root")?.join("fixtures"))
}

/// The run manifests in `dir`, sorted.
fn manifests(dir: &Path) -> Outcome<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.to_string_lossy().ends_with(".run.json") {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

#[test]
fn every_valid_fixture_loads() -> Outcome {
    let valid = manifests(&fixtures()?)?;
    assert!(valid.len() >= 3, "{valid:?}");
    for path in valid {
        let loaded = load_manifest(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(loaded.dir, path.parent().unwrap());
    }
    Ok(())
}

#[test]
fn every_invalid_fixture_is_refused_with_its_code() -> Outcome {
    let invalid = manifests(&fixtures()?.join("invalid"))?;
    assert!(invalid.len() >= 15, "{invalid:?}");
    for path in invalid {
        let err = load_manifest(&path).expect_err(&path.display().to_string());
        let expected_path = PathBuf::from(path.to_string_lossy().replace(".run.json", ".error.json"));
        let expected: Value = serde_json::from_str(&std::fs::read_to_string(&expected_path)?)?;
        let Error::Manifest { source, .. } = &err else {
            panic!("{}: {err:?}", path.display())
        };
        assert_eq!(source.code(), expected["code"], "{}", path.display());
        assert_eq!(source.to_string(), expected["message"], "{}", path.display());
        let runner_code = if expected["code"] == "version" {
            ErrorCode::ManifestVersion
        } else {
            ErrorCode::ManifestInvalid
        };
        assert_eq!(err.code(), runner_code, "{}", path.display());
        assert!(err.source().is_some(), "{}: the manifest's own error is its source", path.display());
    }
    Ok(())
}
