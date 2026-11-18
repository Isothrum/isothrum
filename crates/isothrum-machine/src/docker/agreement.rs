//! Whether Docker Desktop's user has accepted Docker's Subscription Service Agreement, which Docker
//! Desktop shows in its own window at its first start: its settings file records the version of
//! the terms they accepted (`LicenseTermsVersion`). Nothing here ever writes it, nor passes Docker
//! Desktop's installer `--accept-license`: the user accepts it themselves, in Docker's window, and
//! Docker Desktop's engine doesn't start until they have.

use std::path::PathBuf;

use crate::context::Context;

/// The settings key Docker Desktop records the accepted terms' version under (lower-case in the
/// `settings.json` of a Docker Desktop before 4.34).
const KEYS: [&str; 2] = ["LicenseTermsVersion", "licenseTermsVersion"];

/// Whether this computer's Docker Desktop user has accepted its agreement, from its settings
/// ([`crate::host::Devices::docker_desktop_settings`]): `None` when Docker Desktop has written no
/// settings yet (never started) or they don't read as JSON.
#[must_use]
pub fn agreement_accepted(cx: &Context) -> Option<bool> {
    accepted_in(&cx.host.devices.docker_desktop_settings)
}

/// [`agreement_accepted`], from the first of `files` there.
fn accepted_in(files: &[PathBuf]) -> Option<bool> {
    let text = files.iter().find_map(|f| std::fs::read_to_string(f).ok())?;
    let settings: serde_json::Value = serde_json::from_str(&text).ok()?;
    let version = KEYS.iter().find_map(|key| settings.get(key)).and_then(serde_json::Value::as_u64);
    Some(version.is_some_and(|v| v > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What [`accepted_in`] reads of Docker Desktop's folder holding `files` (name, text).
    fn read(files: &[(&str, &str)]) -> Option<bool> {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        accepted_in(&[dir.path().join("settings-store.json"), dir.path().join("settings.json")])
    }

    #[test]
    fn the_agreement_reads_accepted_once_its_terms_version_is_recorded() {
        assert_eq!(
            read(&[("settings-store.json", r#"{"AutoStart": false, "LicenseTermsVersion": 2}"#)]),
            Some(true)
        );
        // Docker Desktop started, its agreement not accepted yet: no version, or 0.
        assert_eq!(read(&[("settings-store.json", r#"{"AutoStart": false}"#)]), Some(false));
        assert_eq!(read(&[("settings-store.json", r#"{"LicenseTermsVersion": 0}"#)]), Some(false));
        // A Docker Desktop before 4.34: its `settings.json`, the key in lower case.
        assert_eq!(read(&[("settings.json", r#"{"licenseTermsVersion": 2}"#)]), Some(true));
    }

    #[test]
    fn no_settings_or_unreadable_ones_say_nothing() {
        assert_eq!(read(&[]), None);
        assert_eq!(read(&[("settings-store.json", "{not json")]), None);
    }
}
