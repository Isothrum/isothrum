//! Lab names come from the editor and become folder names: kebab-case only, nothing else.

/// `name` when it is a lab name (lowercase letters, digits and dashes, at most 63).
pub(crate) fn checked(name: &str) -> Result<&str, String> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok {
        Ok(name)
    } else {
        Err(format!(
            "`{name}` isn't a lab name (lowercase letters, digits and dashes)"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lab_name_is_a_safe_folder_name() {
        assert_eq!(checked("office-lab-2"), Ok("office-lab-2"));
        let long = "a".repeat(64);
        for bad in [
            "",
            "../x",
            "a/b",
            "Lab",
            "a b",
            "a.b",
            "a\\b",
            long.as_str(),
        ] {
            assert!(checked(bad).is_err(), "{bad}");
        }
    }
}
