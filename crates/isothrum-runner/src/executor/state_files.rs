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

/// Writes every state file of `cx`'s operation: a start's, which begins from the manifest.
pub(crate) fn write_all(cx: &Cx<'_>) -> Result<()> {
    write(cx, Keep::Nothing)
}

/// Writes the state files of `cx`'s operation that are missing, keeping the ones there: an
/// operation on an existing run, whose files may carry what the run was started with.
pub(crate) fn write_missing(cx: &Cx<'_>) -> Result<()> {
    write(cx, Keep::Existing)
}

/// What [`write`] leaves as it is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// Every file is written.
    Nothing,
    /// A file already there stays.
    Existing,
}

fn write(cx: &Cx<'_>, keep: Keep) -> Result<()> {
    let dir = cx.state_dir();
    for (path, file) in files(cx) {
        let target = dir.join(&path);
        if keep == Keep::Existing && target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            crate::store::create_dir(parent)?;
        }
        let text = match file.format {
            StateFileFormat::TfvarsJson => tfvars_json(&file.entries),
        };
        crate::store::write(&target, text.as_bytes())?;
    }
    Ok(())
}

/// Removes every state file the manifest names, after a successful `down`.
pub(crate) fn forget(cx: &Cx<'_>) -> Result<()> {
    let dir = cx.state_dir();
    for d in super::every_delivery(cx.manifest()) {
        if let Delivery::StateFile { path, .. } = d {
            let target = dir.join(path.as_str());
            crate::store::remove(&target)?;
        }
    }
    Ok(())
}

/// The entries as one JSON object, a string each, in key order, indented (a JSON value's
/// alternate form, which can't fail as writing through a serializer could).
fn tfvars_json(entries: &BTreeMap<String, String>) -> String {
    let object: serde_json::Map<String, serde_json::Value> = entries.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
    format!("{:#}\n", serde_json::Value::Object(object))
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
