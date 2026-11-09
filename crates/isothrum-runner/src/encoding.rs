//! Encodings the runner writes itself: base64, for a PowerShell script handed to an elevated
//! session and for the values of a guest delivery (one line each, whatever they hold); and a
//! manifest value's name as the manifest writes it.

use serde::Serialize;

/// `value`'s name as the manifest writes it (a target, a provider, an action: `vmware_desktop`),
/// without JSON's quotes; empty for a value that isn't written as a string.
pub(crate) fn wire_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(name)) => name,
        _ => String::new(),
    }
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
    use proptest::prelude::*;

    use super::*;

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
        assert_eq!(wire_name(&3), "", "not written as a string");
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
