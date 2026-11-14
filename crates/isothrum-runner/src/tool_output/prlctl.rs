//! What `prlctl` prints: where `prlctl list --info --json <uuid>` says the VM's bundle is.

/// The bundle folder (`Home`) of the first VM `prlctl list --info --json` printed, without its
/// trailing `/`; `None` when the answer isn't that JSON or names no folder.
pub(crate) fn home(json: &str) -> Option<String> {
    let said: serde_json::Value = serde_json::from_str(json).ok()?;
    let home = said.as_array()?.first()?.get("Home")?.as_str()?;
    Some(home.trim_end_matches('/').to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_vms_home_is_read_without_its_trailing_slash() {
        let said = r#"[{"ID": "{1}", "Home": "/Users/u/Parallels/web.pvm/"}, {"Home": "/other"}]"#;
        assert_eq!(home(said).as_deref(), Some("/Users/u/Parallels/web.pvm"));
        assert_eq!(home("[]"), None);
        assert_eq!(home(r#"[{"ID": "{1}"}]"#), None);
        assert_eq!(home("not json"), None);
    }
}
