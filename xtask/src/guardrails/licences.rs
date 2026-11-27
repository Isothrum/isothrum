//! The licences the release accepts are deny.toml's: `cargo about` (release.yml, which writes
//! each archive's THIRD-PARTY-LICENSES.md) refuses a crate whose licence `.github/release/about.toml`
//! doesn't list, after `cargo deny` let it in. The two lists are one decision, kept equal.

use std::collections::BTreeSet;

/// The strings of the array `key` of the table `table` (the top level when empty) in `text`.
fn listed(text: &str, table: &str, key: &str) -> BTreeSet<String> {
    let doc: toml::Table = text.parse().unwrap_or_else(|e| panic!("not TOML: {e}"));
    let at = if table.is_empty() {
        &doc
    } else {
        doc.get(table).and_then(toml::Value::as_table).unwrap_or_else(|| panic!("no [{table}]"))
    };
    at.get(key)
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("no {key} list"))
        .iter()
        .map(|v| v.as_str().unwrap_or_else(|| panic!("{key}: not a string: {v}")).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_release_accepts_the_licences_deny_allows() {
        let root = crate::repo::root();
        let read = |p: &str| std::fs::read_to_string(root.join(p)).unwrap_or_else(|e| panic!("{p}: {e}"));
        let deny = listed(&read("deny.toml"), "licenses", "allow");
        let about = listed(&read(".github/release/about.toml"), "", "accepted");
        assert_eq!(about, deny, ".github/release/about.toml's `accepted` isn't deny.toml's `allow`");
    }

    #[test]
    fn a_list_is_read_from_its_table() {
        let text = "accepted = [\"MIT\"]\n[licenses]\nallow = [\"ISC\", \"MIT\"]\n";
        assert_eq!(listed(text, "", "accepted"), BTreeSet::from(["MIT".to_owned()]));
        assert_eq!(listed(text, "licenses", "allow"), BTreeSet::from(["ISC".to_owned(), "MIT".to_owned()]));
    }
}
