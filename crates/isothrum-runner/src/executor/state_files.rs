//! Values delivered as a file in the run's state folder (`state_file`): a Terraform variable
//! file (`tfvars_json`, one JSON object), owner-only, every param and slot naming the file merged
//! into it. A start writes them; any other operation
//! writes one only when it is missing, so a destroy reads the values its apply had; a successful
//! `down` removes them.

use std::collections::BTreeMap;

use isoloom_manifest::{Delivery, StateFileFormat};

use super::Cx;
use super::delivery::valued;
use crate::Result;
use crate::error::Error;

/// One file: its format and its entries, by key.
struct File {
    format: StateFileFormat,
    entries: BTreeMap<String, String>,
}

/// The state files `cx`'s operation has values for, by path in the run's state folder.
fn files(cx: &Cx<'_>) -> BTreeMap<String, File> {
    let mut files: BTreeMap<String, File> = BTreeMap::new();
    for (name, value, deliveries) in valued(cx) {
        for d in deliveries {
            if let Delivery::StateFile { path, format, key } = d {
                let file = files.entry(path.as_str().to_owned()).or_insert_with(|| File {
                    format: *format,
                    entries: BTreeMap::new(),
                });
                file.entries.insert(key.clone().unwrap_or_else(|| name.to_owned()), value.clone());
            }
        }
    }
    files
}

/// Writes the state files of `cx`'s operation: every one on a start (`fresh`), else only those
/// missing.
pub(crate) fn write(cx: &Cx<'_>, fresh: bool) -> Result<()> {
    let dir = cx.run.runner.store.run_dir(cx.run.name())?;
    for (path, file) in files(cx) {
        let target = dir.join(&path);
        if !fresh && target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            crate::store::private_dir(parent).map_err(Error::io(parent, "created"))?;
        }
        let text = match file.format {
            StateFileFormat::TfvarsJson => tfvars_json(&file.entries),
        };
        crate::store::write_private(&target, text.as_bytes()).map_err(Error::io(&target, "written"))?;
    }
    Ok(())
}

/// Removes every state file the manifest names, after a successful `down`.
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    let dir = cx.run.runner.store.run_dir(cx.run.name())?;
    for d in super::every_delivery(cx.run.manifest()) {
        if let Delivery::StateFile { path, .. } = d {
            let target = dir.join(path.as_str());
            match std::fs::remove_file(&target) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(Error::io(&target, "removed")(e)),
                _ => {}
            }
        }
    }
    Ok(())
}

/// The entries as one JSON object, a string each, in key order.
fn tfvars_json(entries: &BTreeMap<String, String>) -> String {
    let object: serde_json::Map<String, serde_json::Value> = entries.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    serde_json::to_string_pretty(&serde_json::Value::Object(object)).unwrap_or_default() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tfvars_file_is_one_json_object_of_strings_in_key_order() {
        let entries = BTreeMap::from([("region".to_owned(), "eu-west-3".to_owned()), ("label".to_owned(), "a\"b".to_owned())]);
        assert_eq!(tfvars_json(&entries), "{\n  \"label\": \"a\\\"b\",\n  \"region\": \"eu-west-3\"\n}\n");
    }
}
