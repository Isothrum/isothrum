//! The locale every tool runs in: its messages, numbers and times in the C locale, so what it
//! prints is in the words and formats `tool_output` reads, whatever language the user's session
//! is in; its character set left as the user's (`LC_CTYPE`), so a tool still reads and prints
//! UTF-8 names (a C character set makes Ruby, so Vagrant, read a UTF-8 Vagrantfile as ASCII).
//!
//! `LC_ALL` overrides every category, so a user's `LC_ALL` is moved to `LC_CTYPE`, the one
//! category it keeps deciding. `LANGUAGE`, GNU gettext's list of message languages, is dropped.
//! Windows' tools don't read these variables: nothing is changed there.

use std::ffi::OsString;

/// The categories set to the C locale.
const C_CATEGORIES: [&str; 3] = ["LC_MESSAGES", "LC_NUMERIC", "LC_TIME"];

/// What changes in a tool's environment, given the runner's own variables (`var`): each
/// variable set to its value, or removed (`None`).
pub(super) fn settings(var: impl Fn(&str) -> Option<OsString>) -> Vec<(&'static str, Option<OsString>)> {
    if cfg!(windows) {
        return Vec::new();
    }
    let mut out: Vec<(&'static str, Option<OsString>)> = Vec::new();
    if let Some(all) = var("LC_ALL").filter(|v| !v.is_empty()) {
        out.push(("LC_CTYPE", Some(all)));
    }
    out.push(("LC_ALL", None));
    out.push(("LANGUAGE", None));
    out.extend(C_CATEGORIES.map(|c| (c, Some(OsString::from("C")))));
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn of(vars: &[(&str, &str)]) -> Vec<(&'static str, Option<String>)> {
        settings(|name| vars.iter().find(|(k, _)| *k == name).map(|(_, v)| OsString::from(v)))
            .into_iter()
            .map(|(k, v)| (k, v.map(|v| v.to_string_lossy().into_owned())))
            .collect()
    }

    #[test]
    fn a_french_session_keeps_its_character_set_and_hears_tools_in_c() {
        let set = of(&[("LANG", "fr_FR.UTF-8"), ("LANGUAGE", "fr:en")]);
        let c = Some("C".to_owned());
        assert_eq!(
            set,
            [
                ("LC_ALL", None),
                ("LANGUAGE", None),
                ("LC_MESSAGES", c.clone()),
                ("LC_NUMERIC", c.clone()),
                ("LC_TIME", c)
            ],
            "LANG still gives the character set"
        );
    }

    #[test]
    fn a_users_lc_all_decides_the_character_set_only() {
        let set = of(&[("LC_ALL", "de_DE.UTF-8")]);
        assert_eq!(set.first(), Some(&("LC_CTYPE", Some("de_DE.UTF-8".to_owned()))));
        assert!(set.contains(&("LC_ALL", None)) && set.contains(&("LC_MESSAGES", Some("C".to_owned()))));
        assert_eq!(of(&[("LC_ALL", "")]).first(), Some(&("LC_ALL", None)), "an empty LC_ALL decides nothing");
    }
}
