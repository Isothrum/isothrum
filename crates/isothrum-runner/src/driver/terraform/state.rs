//! What a run's Terraform state says, read from its file only (no Terraform run, so it is cheap
//! to poll): its outputs, whether it holds resources, whether one is tainted.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Deserialize;
use serde_json::Value;

use crate::Result;

/// The state file, in the run's state folder.
pub(crate) const STATE_FILE: &str = "terraform.tfstate";

/// The state Terraform kept before its last write, beside [`STATE_FILE`].
pub(crate) const BACKUP_FILE: &str = "terraform.tfstate.backup";

/// A state, as far as the runner reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TfState {
    /// Each scalar output's value: a string as it is, a number or a boolean as written; an
    /// object, a list or null is none ([`scalar`]).
    pub(super) outputs: BTreeMap<String, String>,
    /// Each map output's entries (`published`: `"web/site" = "10.0.0.5:8080"`).
    pub(super) maps: BTreeMap<String, BTreeMap<String, String>>,
    /// The outputs the module marks sensitive (`sensitive = true`): never shown, whatever the
    /// manifest says of them.
    pub(super) sensitive: BTreeSet<String>,
    /// It holds resources (anything that may cost, or hold a server's names).
    pub(super) resources: bool,
    /// A resource instance is tainted: a create or a provisioner failed.
    pub(super) tainted: bool,
}

/// A state file as Terraform writes it (format version 4), parsed once, here, into what the
/// runner reads of it: its outputs and its resources' instances. Unknown fields are skipped. What
/// a provider writes of an instance (its `attributes`) and what a module outputs stay JSON: their
/// shape is the provider's and the module's.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct StateDoc {
    #[serde(default)]
    outputs: BTreeMap<String, OutputDoc>,
    #[serde(default)]
    pub(super) resources: Vec<ResourceDoc>,
}

/// An output of the state: its value and whether the module marks it sensitive.
#[derive(Debug, Deserialize)]
struct OutputDoc {
    #[serde(default)]
    value: Value,
    #[serde(default)]
    sensitive: bool,
}

/// A resource of the state: a managed one or a data source, of a type, in a module.
#[derive(Debug, Deserialize)]
pub(super) struct ResourceDoc {
    /// `managed` or `data`.
    #[serde(default)]
    pub(super) mode: Option<String>,
    /// Its module's address (`module.lab`); none at the root.
    #[serde(default)]
    pub(super) module: Option<String>,
    /// Its type (`proxmox_virtual_environment_vm`).
    #[serde(rename = "type", default)]
    pub(super) kind: Option<String>,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) instances: Vec<InstanceDoc>,
}

/// An instance of a resource: its key, its status and what its provider wrote of it.
#[derive(Debug, Deserialize)]
pub(super) struct InstanceDoc {
    /// Its `count` or `for_each` key; none for a single instance.
    #[serde(default)]
    pub(super) index_key: Option<IndexKey>,
    /// `tainted` when a create or a provisioner failed.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    attributes: Value,
}

/// An instance's key: a `count` index or a `for_each` key; anything else Terraform doesn't write.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(super) enum IndexKey {
    Number(serde_json::Number),
    Text(String),
    Other(serde::de::IgnoredAny),
}

impl InstanceDoc {
    /// Its attribute `name`, when it is a scalar ([`scalar`]).
    pub(super) fn attribute(&self, name: &str) -> Option<String> {
        self.attributes.get(name).and_then(scalar)
    }
}

impl StateDoc {
    /// Every value the module and the providers wrote into the state: each output's value, each
    /// instance's attributes.
    pub(crate) fn written(&self) -> impl Iterator<Item = &Value> {
        let outputs = self.outputs.values().map(|o| &o.value);
        outputs.chain(self.resources.iter().flat_map(|r| r.instances.iter().map(|i| &i.attributes)))
    }
}

/// Reads the state in `dir`; an empty one when there is none.
pub(super) fn read(dir: &Path) -> Result<TfState> {
    Ok(read_doc(dir)?.map(|doc| of(&doc)).unwrap_or_default())
}

/// The state file in `dir`; `None` when there is none.
///
/// # Errors
///
/// It exists but can't be read, or isn't the JSON Terraform writes (`Corrupt`).
pub(crate) fn read_doc(dir: &Path) -> Result<Option<StateDoc>> {
    crate::store::read_json(&dir.join(STATE_FILE))
}

/// A state file's text, read.
#[cfg(test)]
pub(super) fn parse(text: &str) -> std::result::Result<TfState, serde_json::Error> {
    serde_json::from_str(text).map(|doc: StateDoc| of(&doc))
}

/// What state `doc` says.
fn of(doc: &StateDoc) -> TfState {
    let mut state = TfState::default();
    for (name, o) in &doc.outputs {
        if o.sensitive {
            state.sensitive.insert(name.clone());
        }
        if let Some(map) = o.value.as_object() {
            let scalars = map.iter().filter_map(|(key, field)| Some((key.clone(), scalar(field)?)));
            state.maps.insert(name.clone(), scalars.collect());
        }
        if let Some(s) = scalar(&o.value) {
            state.outputs.insert(name.clone(), s);
        }
    }
    state.resources = !doc.resources.is_empty();
    state.tainted = doc.resources.iter().flat_map(|r| &r.instances).any(|i| i.status.as_deref() == Some("tainted"));
    state
}

/// A scalar value as text (a string as it is); none for null, an object or a list.
fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// The host port of `address` (`10.0.0.5:8080`, `[fd00::1]:8080`).
pub(super) fn port_of(address: &str) -> Option<u16> {
    address.rsplit_once(':')?.1.parse().ok()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn outputs_resources_and_taint_are_read() {
        let s = parse(r#"{"outputs":{"ip":{"value":"10.10.10.5"},"slot":{"value":3},"public":{"value":true},"published":{"value":{"web/site":"10.10.10.5:8080"}},"none":{"value":null}},"resources":[{"instances":[{}]},{"instances":[{"status":"tainted"}]}]}"#).unwrap();
        assert_eq!(s.outputs.get("ip").map(String::as_str), Some("10.10.10.5"));
        assert_eq!(s.outputs.get("slot").map(String::as_str), Some("3"));
        assert_eq!(s.outputs.get("public").map(String::as_str), Some("true"));
        assert!(!s.outputs.contains_key("none") && !s.outputs.contains_key("published"));
        assert_eq!(s.maps["published"]["web/site"], "10.10.10.5:8080");
        assert!(s.resources && s.tainted);
        assert!(s.sensitive.is_empty());
        let secret = parse(r#"{"outputs":{"db_password":{"value":"hunter2","sensitive":true},"url":{"value":"http://x/","sensitive":false}}}"#).unwrap();
        assert_eq!(secret.sensitive.iter().collect::<Vec<_>>(), ["db_password"]);
        let empty = parse(r#"{"version":4,"outputs":{},"resources":[]}"#).unwrap();
        assert!(!empty.resources && !empty.tainted && empty.outputs.is_empty());
    }

    #[test]
    fn a_missing_state_is_an_empty_one_and_a_broken_one_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()).unwrap(), TfState::default());
        std::fs::write(dir.path().join(STATE_FILE), "{").unwrap();
        assert_eq!(read(dir.path()).unwrap_err().code(), crate::ErrorCode::Corrupt);
    }

    #[test]
    fn a_state_terraform_wouldnt_write_is_refused_not_read_empty() {
        assert!(parse(r#"{"resources":{}}"#).is_err(), "resources not a list");
        assert!(parse(r#"{"outputs":{"ip":{"value":"x","sensitive":"yes"}}}"#).is_err());
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), r#"{"resources":[{"instances":7}]}"#).unwrap();
        assert_eq!(read(dir.path()).unwrap_err().code(), crate::ErrorCode::Corrupt);
    }

    #[test]
    fn a_state_that_cannot_be_read_is_an_error_not_an_empty_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(STATE_FILE)).unwrap();
        assert_eq!(read(dir.path()).unwrap_err().code(), crate::ErrorCode::Io);
    }

    #[test]
    fn a_published_address_gives_its_port() {
        assert_eq!(port_of("10.10.10.5:8080"), Some(8080));
        assert_eq!(port_of("[fd00::1]:443"), Some(443));
        assert_eq!(port_of("10.10.10.5"), None);
    }

    proptest! {
        #[test]
        fn any_text_reads_or_fails_without_panicking(text in ".{0,200}") {
            // Read or refused, never a panic; a tainted instance is always one of its resources.
            if let Ok(state) = parse(&text) {
                prop_assert!(!state.tainted || state.resources);
            }
        }
    }
}
