//! What the host's tools say of themselves, read in one place for the runner and the
//! machine-setup backend (`isothrum-machine`): the version a tool prints, and the plugins
//! `vagrant plugin list --machine-readable` lists.

/// The first version in `text` (`v2.29.1`, `Docker Compose version 5.5.1`, `0.2.0-beta`), as its
/// major, minor and patch numbers (a missing patch is 0); `None` when `text` holds no
/// `<major>.<minor>`. Versions compare as these tuples do.
#[must_use]
pub fn version_in(text: &str) -> Option<(u64, u64, u64)> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.')).find_map(|word| {
        let mut parts = word.split('.').filter(|p| !p.is_empty()).map(str::parse::<u64>);
        let major = parts.next()?.ok()?;
        let minor = parts.next()?.ok()?;
        let patch = parts.next().and_then(Result::ok).unwrap_or(0);
        Some((major, minor, patch))
    })
}

/// Each plugin `vagrant plugin list --machine-readable` names (its `plugin-name` lines), with its
/// version when a `plugin-version` line gives one (`0.12.2%!(VAGRANT_COMMA) global`: the version
/// before Vagrant's escaped comma).
#[must_use]
pub fn vagrant_plugins(out: &str) -> Vec<(String, Option<String>)> {
    let lines: Vec<(&str, &str, &str)> = out.lines().filter_map(vagrant_fields).collect();
    let version = |name: &str| {
        lines
            .iter()
            .find(|(target, kind, _)| *target == name && *kind == "plugin-version")
            .and_then(|(_, _, data)| {
                let v = data.split("%!(VAGRANT_COMMA)").next().unwrap_or_default().trim();
                (!v.is_empty()).then(|| v.to_owned())
            })
    };
    lines
        .iter()
        .filter(|(_, kind, data)| *kind == "plugin-name" && !data.is_empty())
        .map(|(_, _, name)| ((*name).to_owned(), version(name)))
        .collect()
}

/// One line of Vagrant's `--machine-readable` output: its target, type and data (the rest of the
/// line). Lines are `timestamp,target,type,data…`; Vagrant writes a comma in a field as
/// `%!(VAGRANT_COMMA)`, so the data is kept whole as printed.
pub(crate) fn vagrant_fields(line: &str) -> Option<(&str, &str, &str)> {
    let mut parts = line.trim_end_matches('\r').splitn(4, ',');
    let (_ts, target, kind, data) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    Some((target, kind, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_what_a_tool_prints() {
        assert_eq!(version_in("5.5.1"), Some((5, 5, 1)));
        assert_eq!(version_in("v2.29.1-desktop.1"), Some((2, 29, 1)));
        assert_eq!(version_in("Docker Compose version 2.17"), Some((2, 17, 0)));
        assert_eq!(version_in("29.8.1"), Some((29, 8, 1)));
        assert_eq!(version_in("0.2.0-beta"), Some((0, 2, 0)), "a suffix after the numbers is dropped");
        assert_eq!(version_in("no version here"), None);
        assert!(version_in("2.16.9") < version_in("2.17.0"));
        assert!(version_in("4.7") > version_in("4.6.5"), "a missing part is 0");
    }

    #[test]
    fn plugins_are_read_by_name_with_their_versions() {
        // As Vagrant 2.4 prints them (a `ui` line repeats the human list).
        let out = "1794305819,,ui,info,vagrant-qemu (0.7.0%!(VAGRANT_COMMA) global)\n\
                   1794305819,,plugin-name,vagrant-qemu\n\
                   1794305819,vagrant-qemu,plugin-version,0.7.0%!(VAGRANT_COMMA) global\n\
                   1794305819,,plugin-name,vagrant-vmware-esxi\r\n\
                   1794305819,,plugin-name,vagrant_utm\n\
                   1794305819,vagrant_utm,plugin-version,0.2.0\n";
        assert_eq!(
            vagrant_plugins(out),
            [
                ("vagrant-qemu".to_owned(), Some("0.7.0".to_owned())),
                ("vagrant-vmware-esxi".to_owned(), None),
                ("vagrant_utm".to_owned(), Some("0.2.0".to_owned())),
            ]
        );
        assert_eq!(vagrant_plugins(""), Vec::<(String, Option<String>)>::new());
    }
}
