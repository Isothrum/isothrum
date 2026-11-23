//! What the Azure CLI says, read: `az version --output json`'s own version.

/// The Azure CLI's version, the `azure-cli` entry of the JSON `az version --output json` prints;
/// none when the answer isn't that JSON or the entry isn't text. A `--query` would need quotes,
/// which can't pass through `cmd.exe` (`az` is a `.cmd`).
#[must_use]
pub fn version(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json.trim()).ok()?;
    value.get("azure-cli")?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn the_version_is_the_azure_cli_entry_of_its_json() {
        assert_eq!(version(include_str!("fixtures/az/2.90.0-version.json")).as_deref(), Some("2.90.0"));
        assert_eq!(version(include_str!("fixtures/az/2.66.0-windows-version.json")).as_deref(), Some("2.66.0"));
        assert_eq!(version("WARNING: something\n{}"), None);
        assert_eq!(version("{\"azure-cli\": 2.9}"), None, "a version that isn't text");
        assert_eq!(version("not json"), None);
        assert_eq!(version(""), None);
    }

    proptest! {
        #[test]
        fn only_the_azure_cli_string_is_read(
            noise in "(?s).{0,80}",
            v in "\\PC{0,12}",
            extra in proptest::collection::btree_map("[a-z-]{1,12}", "\\PC{0,8}", 0..4),
            pad in "[ \n\r\t]{0,3}",
        ) {
            // Any text reads without a panic.
            let _read: Option<String> = version(&noise);
            let mut doc = serde_json::json!(extra);
            doc["azure-cli"] = serde_json::json!(v);
            let text = format!("{pad}{}{pad}", serde_json::to_string_pretty(&doc).unwrap());
            prop_assert_eq!(version(&text), Some(v));
            doc["azure-cli"] = serde_json::json!(1);
            prop_assert_eq!(version(&doc.to_string()), None);
        }
    }
}
