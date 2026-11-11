//! The duplicate-code check: jscpd over the Rust sources (every crate, the fuzz crate's targets and
//! the xtask), each block found twice (at least 50 tokens over 5 lines) counted in the crate of its
//! first copy. It runs a pinned jscpd through `npx`, so it needs Node.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use super::{Counts, Crate, add, crate_of};
use crate::{Res, process, repo};

/// The jscpd release the counts are measured with.
const JSCPD: &str = "jscpd@5.4.1";

/// Runs jscpd and counts its clones per crate.
pub(crate) fn measure(root: &Path, crates: &[Crate], counts: &mut Counts) -> Res<()> {
    let out = std::env::temp_dir().join(format!("isothrum-jscpd-{}", std::process::id()));
    let npx = process::program("npx", &std::env::var_os("PATH").unwrap_or_default());
    let mut cmd = Command::new(npx);
    cmd.args(["--yes", JSCPD, "--silent", "--absolute", "-f", "rust", "-r", "json", "-o"]).arg(&out);
    cmd.args(["crates", "fuzz/fuzz_targets", "xtask/src"]).current_dir(root);
    let ran = process::quiet(&mut cmd).map_err(|e| format!("jscpd (through npx, which needs Node): {e}"));
    let report = ran.and_then(|()| std::fs::read_to_string(out.join("jscpd-report.json")).map_err(|e| e.to_string()));
    // The report folder goes; one left behind in the temporary folder is harmless.
    repo::remove(&out).ok();
    let report: Value = serde_json::from_str(&report?)?;
    let base = format!("{}/", repo::slashed(root));
    for clone in report.get("duplicates").and_then(Value::as_array).into_iter().flatten() {
        let file = repo::slashed(Path::new(first_file(clone)?));
        add(counts, "dup/clone", &crate_of(crates, file.strip_prefix(&base).unwrap_or(&file)));
    }
    Ok(())
}

/// The file a clone of jscpd's report is first found in; a clone that names none is an error, not
/// one counted in no crate.
fn first_file(clone: &Value) -> Res<&str> {
    Ok(clone
        .get("firstFile")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("jscpd's report has a clone with no first file: {clone}"))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a clone with no file name was counted under `workspace`, a crate of none.
    #[test]
    fn a_clone_with_no_file_fails_the_count() {
        let named = serde_json::json!({"firstFile": {"name": "/w/crates/a/src/lib.rs"}});
        assert_eq!(first_file(&named).unwrap(), "/w/crates/a/src/lib.rs");
        assert!(first_file(&serde_json::json!({"secondFile": {"name": "x"}})).is_err());
    }
}
