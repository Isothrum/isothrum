//! What `VBoxManage showvminfo --machinereadable` prints, read. Its fixture is in
//! `fixtures/vboxmanage/`.

/// The value of `key` in `showvminfo --machinereadable`'s output (`key="value"` lines, or a bare
/// number), its escapes undone.
pub(super) fn value(info: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    info.lines().find_map(|l| {
        let raw = l.trim_end_matches('\r').strip_prefix(&prefix)?;
        Some(match raw.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            Some(quoted) => unescape(quoted),
            None => raw.to_owned(),
        })
    })
}

/// A quoted value's escapes undone (`\\` is `\`, `\"` is `"`).
fn unescape(quoted: &str) -> String {
    let mut out = String::with_capacity(quoted.len());
    let mut chars = quoted.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(next @ ('\\' | '"'))) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Windows path in a description is written with its `\` doubled.
    #[test]
    fn machine_readable_values_are_unescaped() {
        let info = include_str!("fixtures/vboxmanage/7.1.4-windows-showvminfo.txt");
        assert_eq!(
            value(info, "description").as_deref(),
            Some("isothrum-selftest:C:\\Users\\ada\\AppData\\Local\\isothrum\\selftest\\vm")
        );
        assert_eq!(value(info, "name").as_deref(), Some("isothrum-selftest"));
        assert_eq!(value(info, "memory").as_deref(), Some("1024"), "a bare number");
        assert_eq!(value(info, "missing"), None);
        assert_eq!(value("description=\"say \\\"hi\\\"\"", "description").as_deref(), Some("say \"hi\""));
    }
}
