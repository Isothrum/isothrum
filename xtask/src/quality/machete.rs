//! Unused dependencies: what `cargo machete` reports, and what a crate tells it to ignore
//! (`[package.metadata.cargo-machete] ignored`), both counted per crate.

use std::path::Path;
use std::process::Output;

use super::{Counts, Crate, add};
use crate::Res;
use crate::repo::path_in;

/// Counts the unused and the ignored dependencies per crate.
pub(crate) fn measure(root: &Path, crates: &[Crate], counts: &mut Counts) -> Res<()> {
    let out = crate::process::cargo(root)
        .arg("machete")
        .output()
        .map_err(|e| format!("cargo machete couldn't start (cargo install cargo-machete): {e}"))?;
    for name in unused(&report(&out)?) {
        add(counts, "machete/unused", &name);
    }
    for c in crates {
        let path = path_in(root, &c.dir).join("Cargo.toml");
        let manifest = std::fs::read_to_string(&path)?;
        let count = ignored(&manifest).map_err(|e| format!("{}: {e}", path.display()))?;
        for _ in 0..count {
            add(counts, "machete/ignored", &c.name);
        }
    }
    Ok(())
}

/// Machete's report, when it ran to the end: it exits 0 when it found nothing unused and 1 when
/// it found some. Any other exit (a manifest it couldn't read, a crash) is an error, with what it
/// said, since its report would count nothing.
fn report(out: &Output) -> Res<String> {
    match out.status.code() {
        Some(0 | 1) => Ok(String::from_utf8_lossy(&out.stdout).into_owned()),
        _ => Err(format!("cargo machete failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()).into()),
    }
}

/// The crate of each unused dependency in machete's report (`<crate> -- <manifest>:` then one
/// indented line per dependency).
fn unused(report: &str) -> Vec<String> {
    let mut current = None;
    let mut out = Vec::new();
    for line in report.lines() {
        if let Some((name, _)) = line.split_once(" -- ") {
            current = Some(name.trim().to_owned());
        } else if line.starts_with(['\t', ' ']) && !line.trim().is_empty() {
            out.extend(current.clone());
        } else {
            current = None;
        }
    }
    out
}

/// How many dependencies a manifest tells machete to ignore; a manifest that isn't TOML is an
/// error, not one that ignores nothing.
fn ignored(manifest: &str) -> Res<usize> {
    let doc = manifest.parse::<toml::Table>()?;
    Ok(doc
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("cargo-machete"))
        .and_then(|m| m.get("ignored"))
        .and_then(toml::Value::as_array)
        .map_or(0, Vec::len))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a command exiting with `code` printed.
    #[cfg(unix)]
    fn exited(code: i32, stdout: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.as_bytes().to_vec(),
            stderr: b"error: couldn't read Cargo.toml\n".to_vec(),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_report_is_read_when_machete_found_nothing_or_something_and_else_is_an_error() {
        assert_eq!(report(&exited(0, "Done!\n")).unwrap(), "Done!\n");
        assert_eq!(report(&exited(1, "a -- ./Cargo.toml:\n\tserde\n")).unwrap(), "a -- ./Cargo.toml:\n\tserde\n");
        let failed = report(&exited(2, "")).unwrap_err().to_string();
        assert!(
            failed.contains("cargo machete failed") && failed.contains("couldn't read Cargo.toml"),
            "{failed}"
        );
    }

    #[test]
    fn each_unused_dependency_is_named_with_its_crate() {
        let report = "cargo-machete found the following unused dependencies in this directory:\nisothrum-x -- ./crates/isothrum-x/Cargo.toml:\n\tserde\n\tindexmap\n\nDone!\n";
        assert_eq!(unused(report), ["isothrum-x", "isothrum-x"]);
        assert_eq!(
            ignored("[package]\nname = \"a\"\n[package.metadata.cargo-machete]\nignored = [\"indexmap\"]\n").unwrap(),
            1
        );
        assert_eq!(ignored("[package]\nname = \"a\"\n").unwrap(), 0);
    }

    /// Regression: a manifest that wouldn't parse counted as one ignoring nothing.
    #[test]
    fn a_manifest_that_isnt_toml_fails_the_count() {
        assert!(ignored("[package\nname = ").is_err());
    }
}
