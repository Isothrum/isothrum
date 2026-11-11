//! What the host's tools say of themselves, read in one place for the runner and the
//! machine-setup backend (`isothrum-machine`): the version a tool prints, in Isoloom's order, the
//! plugins `vagrant plugin list --machine-readable` lists, and the error a
//! machine-readable Vagrant call failed with.

use isoloom_manifest::Version;

/// The version `text` says (what a tool prints: `Vagrant 2.4.1`, `v2.29.1`, `1.0.0-rc.1`), as
/// Isoloom's [`Version`], so versions compare in its order (a pre-release before its release):
/// its first word that is one, else its first `<major>.<minor>[.<patch>]` (`2.3.8.dev` is 2.3.8, a
/// missing patch 0). `None` when it says none: nothing is held back then. The one reading of a
/// version for the runner's tool probes and `isothrum-machine`'s minimums.
#[must_use]
pub fn version_found(text: &str) -> Option<Version> {
    text.split(|c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')'))
        .map(|w| w.strip_prefix('v').unwrap_or(w))
        .find_map(|w| Version::parse(w).ok())
        .or_else(|| {
            // Isoloom's Version is made from its text alone.
            let (major, minor, patch) = numbers_in(text)?;
            Version::parse(&format!("{major}.{minor}.{patch}")).ok()
        })
}

/// The first `<major>.<minor>[.<patch>]` in `text` (`Docker Compose version 2.17`,
/// `2.3.8.dev`), its suffix dropped and a missing patch 0.
fn numbers_in(text: &str) -> Option<(u64, u64, u64)> {
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

/// The error a Vagrant call printed as its `--machine-readable` `error-exit` record among `stdout`
/// (its last, when there are more): its message as Vagrant words it for a person, followed by
/// its class (`… because it is locked! … (Vagrant::Errors::MachineLocked)`). With
/// `--machine-readable`, Vagrant prints its error there, on standard output, and nothing of it on
/// standard error.
pub(crate) fn vagrant_error(stdout: &[String]) -> Option<String> {
    let data = stdout
        .iter()
        .rev()
        .filter_map(|line| vagrant_fields(line))
        .find_map(|(_, kind, data)| (kind == "error-exit").then_some(data))?;
    let (class, message) = data.split_once(',').unwrap_or((data, ""));
    let (class, message) = (unescape(class), unescape(message));
    match (class.trim(), message.trim()) {
        ("", "") => None,
        (class, "") => Some(class.to_owned()),
        ("", message) => Some(message.to_owned()),
        (class, message) => Some(format!("{message} ({class})")),
    }
}

/// A `--machine-readable` field as Vagrant wrote it, its escapes undone: a comma written as
/// `%!(VAGRANT_COMMA)`, a line break as `\n`, a carriage return as `\r`.
fn unescape(field: &str) -> String {
    field.replace("%!(VAGRANT_COMMA)", ",").replace("\\r", "").replace("\\n", "\n")
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

    /// A version as each tool prints it, compared in Isoloom's order: by its numbers (a missing
    /// patch 0), a pre-release before its release.
    #[test]
    fn versions_are_read_from_what_a_tool_prints_in_isoloom_s_order() {
        let v = |text: &str| version_found(text).unwrap();
        let parsed = |text: &str| Version::parse(text).unwrap();
        assert_eq!(v("5.5.1"), parsed("5.5.1"));
        assert_eq!(v("Vagrant 2.4.1"), parsed("2.4.1"));
        assert_eq!(v("v2.29.1-desktop.1"), parsed("2.29.1-desktop.1"));
        assert_eq!(v("Docker Compose version 2.17"), parsed("2.17.0"));
        assert_eq!(v("2.3.8.dev"), parsed("2.3.8"), "a suffix that isn't a pre-release is dropped");
        assert_eq!(version_found("no version here"), None);
        assert!(v("2.16.9") < v("2.17.0"));
        assert!(v("4.7") > v("4.6.5"), "a missing part is 0");
        assert!(v("4.7.5") > v("4.6.5") && v("4.6.5") >= v("4.6.5"));
        assert!(v("0.1.6") < v("0.2.0"));
        assert!(v("0.2.0-beta") < v("0.2.0"), "a pre-release comes before its release");
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

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_machine_readable_error_is_read_whole_with_its_class() {
        // As Vagrant 2.4.9 prints it on libvirt while another process holds the machine's lock.
        let said = "Vagrant can't use the requested machine because it is locked! This\\nmeans that another Vagrant \
                    process is currently reading or modifying\\nthe machine.\\n\\nName: probe\\nProvider: libvirt";
        let out = lines(&format!(
            "1794342958,,ui,error,{said}\n1794342958,,error-exit,Vagrant::Errors::MachineLocked,{said}\n"
        ));
        assert_eq!(
            vagrant_error(&out).as_deref(),
            Some(
                "Vagrant can't use the requested machine because it is locked! This\nmeans that another Vagrant \
                 process is currently reading or modifying\nthe machine.\n\nName: probe\nProvider: libvirt \
                 (Vagrant::Errors::MachineLocked)"
            )
        );
        let commas = lines("1,,error-exit,Vagrant::Errors::VagrantError,a%!(VAGRANT_COMMA) b\\r\\n");
        assert_eq!(vagrant_error(&commas).as_deref(), Some("a, b (Vagrant::Errors::VagrantError)"));
        assert_eq!(
            vagrant_error(&lines("1,,error-exit,Vagrant::Errors::X,")).as_deref(),
            Some("Vagrant::Errors::X")
        );
        assert_eq!(vagrant_error(&lines("1,web,state,running\n==> web: up")), None);
        assert_eq!(vagrant_error(&lines("1,,error-exit,")), None);
    }
}
