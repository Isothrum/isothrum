//! What a run's Terraform state says, read from its file only (no Terraform run, so it is cheap
//! to poll): its outputs, whether it holds resources, whether one is tainted. Ported from the
//! launcher's `runtime/terraform/state.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::Result;
use crate::error::Error;

/// The state file, in the run's state folder.
pub(super) const STATE_FILE: &str = "terraform.tfstate";

/// A state, as far as the runner reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TfState {
    /// Each output's value: a string as it is, anything else as JSON.
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

/// Reads the state in `dir`; an empty one when there is none.
pub(super) fn read(dir: &Path) -> Result<TfState> {
    let path = dir.join(STATE_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|e| Error::io(&path, "read")(std::io::Error::new(std::io::ErrorKind::InvalidData, e))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TfState::default()),
        Err(e) => Err(Error::io(&path, "read")(e)),
    }
}

/// A state file's text, read.
pub(super) fn parse(text: &str) -> std::result::Result<TfState, serde_json::Error> {
    let v: Value = serde_json::from_str(text)?;
    let mut state = TfState::default();
    if let Some(outputs) = v.get("outputs").and_then(Value::as_object) {
        for (name, o) in outputs {
            let value = o.get("value").unwrap_or(&Value::Null);
            if o.get("sensitive").and_then(Value::as_bool) == Some(true) {
                state.sensitive.insert(name.clone());
            }
            if let Some(map) = value.as_object() {
                let scalars = map.iter().filter_map(|(key, field)| Some((key.clone(), scalar(field)?)));
                state.maps.insert(name.clone(), scalars.collect());
            }
            if let Some(s) = scalar(value) {
                state.outputs.insert(name.clone(), s);
            }
        }
    }
    let resources = v.get("resources").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    state.resources = !resources.is_empty();
    state.tainted = resources
        .iter()
        .flat_map(|r| r.get("instances").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default())
        .any(|i| i.get("status").and_then(Value::as_str) == Some("tainted"));
    Ok(state)
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
        assert_eq!(read(dir.path()).unwrap_err().code(), crate::ErrorCode::Io);
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
