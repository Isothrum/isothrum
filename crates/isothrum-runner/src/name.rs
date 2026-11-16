//! A run's name: what the embedder calls the run (`app-web-portal`), else the run's name.
//! It names the run's state folder and, through `${run.name}`, the Compose project and the
//! networks, so it holds only what every engine accepts.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Result;
use crate::error::Error;

/// The longest name: Docker's limit for a project name, minus room for a network suffix.
const MAX_LEN: usize = 63;

/// A run's name: 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit
/// (Compose's rule for a project name), and not a Windows device name (`con`, `nul`, `com1`…),
/// which can't name a folder there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RunName(String);

impl RunName {
    /// The name, when it can name a run.
    ///
    /// # Errors
    ///
    /// Fails with `run_name` when `value` breaks that rule.
    pub fn new(value: &str) -> Result<RunName> {
        let mut chars = value.chars();
        let first = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        let rest = chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
        if first && rest && value.len() <= MAX_LEN && !is_windows_device(value) {
            Ok(RunName(value.to_owned()))
        } else {
            Err(Error::RunName { value: value.to_owned() })
        }
    }

    /// The name a project in folder `dir` runs as when none is given: the folder's name in
    /// lower case, each character a run name can't hold as `-`, cut to the longest a name takes
    /// (Compose names a project after its folder the same way).
    ///
    /// # Errors
    ///
    /// `run_name` when nothing of the folder's name is left that can name a run.
    pub fn from_folder(dir: &std::path::Path) -> Result<RunName> {
        let folder = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let made: String = folder
            .chars()
            .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' { c } else { '-' })
            .collect();
        let trimmed: String = made.trim_start_matches(['-', '_']).chars().take(MAX_LEN).collect();
        RunName::new(&trimmed).map_err(|_| Error::RunName { value: folder })
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether `value` is one of the names Windows keeps for a device: a folder can't take it, on
/// any drive, so a run named so couldn't keep its state there. A run name has no `.`, so the
/// name alone is what counts.
fn is_windows_device(value: &str) -> bool {
    match value.as_bytes() {
        b"con" | b"prn" | b"aux" | b"nul" => true,
        [b'c', b'o', b'm', n] | [b'l', b'p', b't', n] => n.is_ascii_digit(),
        _ => false,
    }
}

impl fmt::Display for RunName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RunName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RunName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<RunName, D::Error> {
        RunName::new(&String::deserialize(d)?).map_err(serde::de::Error::custom)
    }
}

impl std::str::FromStr for RunName {
    type Err = Error;

    fn from_str(s: &str) -> Result<RunName> {
        RunName::new(s)
    }
}

impl TryFrom<&str> for RunName {
    type Error = Error;

    fn try_from(value: &str) -> Result<RunName> {
        RunName::new(value)
    }
}

impl TryFrom<String> for RunName {
    type Error = Error;

    fn try_from(value: String) -> Result<RunName> {
        RunName::new(&value)
    }
}

impl AsRef<str> for RunName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_name_is_a_compose_project_name() {
        for good in ["web-portal", "app-invoice-portal-api", "a", "stack_2", "9lives"] {
            assert_eq!(RunName::new(good).unwrap().as_str(), good);
        }
        for bad in ["", "Web", "-x", "_x", "a b", "a/b", "..", "é", &"a".repeat(64)] {
            assert_eq!(RunName::new(bad).unwrap_err().code(), crate::ErrorCode::RunName, "{bad}");
        }
    }

    #[test]
    fn a_windows_device_name_is_no_run_name() {
        for device in ["con", "prn", "aux", "nul", "com0", "com1", "com9", "lpt1", "lpt9"] {
            assert_eq!(RunName::new(device).unwrap_err().code(), crate::ErrorCode::RunName, "{device}");
        }
        for good in ["console", "con-1", "nul_stack", "com10", "lpt", "com", "auxiliary", "xcon"] {
            assert_eq!(RunName::new(good).unwrap().as_str(), good);
        }
    }

    #[test]
    fn a_run_name_reads_back_checked() {
        let name: RunName = serde_json::from_str("\"web-1\"").unwrap();
        assert_eq!(serde_json::to_string(&name).unwrap(), "\"web-1\"");
        let err = serde_json::from_str::<RunName>("\"../etc\"").unwrap_err().to_string();
        assert!(err.contains("../etc"), "the refused name is said: {err}");
    }
}
