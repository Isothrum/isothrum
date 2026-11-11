//! Encodings the runner writes itself: base64, for a PowerShell script handed to an elevated
//! session and for the values of a guest delivery (one line each, whatever they hold); and a
//! manifest value's name as the manifest writes it, both ways ([`wire_name`], [`from_wire_name`],
//! [`every_name`]: the helpers every crate of the workspace names and reads such a value with).

use serde::Serialize;
use serde::de::DeserializeOwned;

/// `value`'s name as the manifest and the event stream write it (a target, a provider, a
/// machine's role: `vmware_desktop`), without JSON's quotes. It is meant for the unit enums serde
/// writes as a string; any other value is named by its JSON text (`3`), and one serde can't write
/// by why, so a name is never silently empty.
#[must_use]
pub fn wire_name<T: Serialize + ?Sized>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(e) => format!("<{e}>"),
    }
}

/// The value of the unit enum `T` that the manifest writes as `name` (`vmware_desktop`): the way
/// back from [`wire_name`]. `None` when no value of `T` has that name.
#[must_use]
pub fn from_wire_name<T: DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
}

/// Every name the unit enum `T` is written as, from its JSON Schema (each variant's `const`, or
/// the `enum` that lists them), in the schema's order: the values a command line offers for a type
/// of Isoloom's, so a variant added there is offered without a list of its own to keep in step.
#[must_use]
pub fn every_name<T: schemars::JsonSchema>() -> Vec<String> {
    fn names(schema: &serde_json::Value, out: &mut Vec<String>) {
        match schema {
            serde_json::Value::Object(fields) => {
                if let Some(serde_json::Value::String(name)) = fields.get("const") {
                    out.push(name.clone());
                }
                if let Some(serde_json::Value::Array(listed)) = fields.get("enum") {
                    out.extend(listed.iter().filter_map(|v| v.as_str().map(str::to_owned)));
                }
                fields.values().for_each(|v| names(v, out));
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| names(v, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    names(schemars::schema_for!(T).as_value(), &mut found);
    found
}

/// Standard base64 (RFC 4648, padded).
pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                let index = usize::try_from((n >> shift) & 63).unwrap_or(0);
                out.push(char::from(TABLE.get(index).copied().unwrap_or(b'A')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use isoloom_manifest::Provider;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_name_reads_back_as_its_value_and_every_name_is_one() {
        let names = every_name::<Provider>();
        assert!(names.contains(&"vmware_esxi".to_owned()), "{names:?}");
        for name in &names {
            let provider: Provider = from_wire_name(name).unwrap();
            assert_eq!(&wire_name(&provider), name);
        }
        assert_eq!(from_wire_name::<Provider>("VirtualBox"), None);
        assert_eq!(from_wire_name::<Provider>(""), None);
    }

    /// RFC 4648 decoding, for the tests: what `base64 -d` does.
    fn decoded(text: &str) -> Vec<u8> {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let values: Vec<u32> = text
            .bytes()
            .filter(|b| *b != b'=')
            .map(|b| u32::try_from(TABLE.iter().position(|t| *t == b).unwrap()).unwrap())
            .collect();
        let mut out = Vec::new();
        for chunk in values.chunks(4) {
            let n = chunk.iter().zip([18u32, 12, 6, 0]).fold(0u32, |n, (v, shift)| n | (v << shift));
            let bytes = n.to_be_bytes();
            out.extend_from_slice(bytes.get(1..chunk.len()).unwrap());
        }
        out
    }

    #[test]
    fn rfc_4648_examples_encode_as_written() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn a_manifest_value_is_named_without_json_quotes() {
        assert_eq!(wire_name(&isoloom_manifest::Provider::VmwareDesktop), "vmware_desktop");
        assert_eq!(wire_name(&isoloom_manifest::Target::Docker), "docker");
        assert_eq!(wire_name(&3), "3", "not written as a string: its JSON text");
        let map_with_a_list_key = std::collections::BTreeMap::from([(vec![1], 1)]);
        assert!(wire_name(&map_with_a_list_key).starts_with("<key must be a string"), "named by why");
    }

    proptest! {
        #[test]
        fn any_bytes_read_back_as_themselves(bytes in proptest::collection::vec(any::<u8>(), 0..64)) {
            let text = base64(&bytes);
            prop_assert!(text.len().is_multiple_of(4) && !text.contains(['\n', ' ']));
            prop_assert_eq!(decoded(&text), bytes);
        }
    }
}
