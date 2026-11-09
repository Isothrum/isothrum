//! A run's name: what the embedder calls the run (`app-flag-portal`), else the run's name.
//! It names the run's state folder and, through `${run.name}`, the Compose project and the
//! networks, so it holds only what every engine accepts.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Result;
use crate::error::Error;

/// The longest name: Docker's limit for a project name, minus room for a network suffix.
const MAX_LEN: usize = 63;

/// A run's name: 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit
/// (Compose's rule for a project name).
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
        if first && rest && value.len() <= MAX_LEN {
            Ok(RunName(value.to_owned()))
        } else {
            Err(Error::RunName { value: value.to_owned() })
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
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
        for good in ["flag-portal", "app-invoice-portal-api", "a", "lab_2", "9lives"] {
            assert_eq!(RunName::new(good).unwrap().as_str(), good);
        }
        for bad in ["", "Flag", "-x", "_x", "a b", "a/b", "..", "é", &"a".repeat(64)] {
            assert_eq!(RunName::new(bad).unwrap_err().code(), crate::ErrorCode::RunName, "{bad}");
        }
    }

    #[test]
    fn a_run_name_reads_back_checked() {
        let name: RunName = serde_json::from_str("\"web-1\"").unwrap();
        assert_eq!(serde_json::to_string(&name).unwrap(), "\"web-1\"");
        assert!(serde_json::from_str::<RunName>("\"../etc\"").is_err());
    }
}
