//! `schema`: the JSON Schemas of what the runner writes for other programs, committed beside the
//! runner so embedders and CI jobs in other languages read it without the crate: the event stream
//! (`--json`) and a run's status (`status --json`). A test fails when a committed file is stale.

use std::path::{Path, PathBuf};

use crate::{Res, repo};

/// Each schema file, relative to the repository, with the schema it holds.
const SCHEMAS: [(&str, fn() -> serde_json::Value); 2] = [
    ("crates/isothrum-runner/schema/events-v0.schema.json", isothrum_runner::events_schema),
    ("crates/isothrum-runner/schema/status.schema.json", isothrum_runner::status_schema),
];

/// A schema's path in the checkout at `root`.
fn path(root: &Path, file: &str) -> PathBuf {
    repo::path_in(root, file)
}

/// A schema, as committed.
fn text(schema: fn() -> serde_json::Value) -> Res<String> {
    Ok(serde_json::to_string_pretty(&schema())? + "\n")
}

/// Writes every schema.
pub(crate) fn write() -> Res<()> {
    let root = repo::root();
    for (file, schema) in SCHEMAS {
        let path = path(&root, file);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, text(schema)?).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_schemas_are_current() {
        for (file, schema) in SCHEMAS {
            let committed = std::fs::read_to_string(path(&repo::root(), file)).unwrap_or_default();
            assert!(committed == text(schema).unwrap(), "{file} is stale: run `cargo run -p xtask -- schema`");
        }
    }
}
