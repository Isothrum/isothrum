//! Sensitive values: held by the embedder, handed in per operation, never printed, never stored,
//! and never written to a file: the tools get them in their environment
//! ([`crate::project::inputs`]), and every event masks them.

use std::collections::BTreeMap;
use std::fmt;

/// A secret's value. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Holds `value`.
    pub fn new(value: impl Into<String>) -> SecretValue {
        SecretValue(value.into())
    }

    /// The value itself, for the tool it is handed to.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

/// Sensitive values, by the name the tools get each under.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Secrets(BTreeMap<String, SecretValue>);

impl Secrets {
    /// No values.
    #[must_use]
    pub fn new() -> Secrets {
        Secrets::default()
    }

    /// Sets `name`'s value.
    pub fn insert(&mut self, name: impl Into<String>, value: SecretValue) {
        self.0.insert(name.into(), value);
    }

    /// `name`'s value, when given.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SecretValue> {
        self.0.get(name)
    }

    /// Every value given, by name.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &SecretValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}

impl From<String> for SecretValue {
    fn from(value: String) -> SecretValue {
        SecretValue(value)
    }
}

impl From<&str> for SecretValue {
    fn from(value: &str) -> SecretValue {
        SecretValue(value.to_owned())
    }
}

impl<K: Into<String>, V: Into<SecretValue>> FromIterator<(K, V)> for Secrets {
    fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Secrets {
        Secrets(iter.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints() {
        let mut s = Secrets::new();
        s.insert("DEMO_SECRET", SecretValue::new("token-hunter2"));
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2") && shown.contains("<redacted>"), "{shown}");
        assert_eq!(s.get("DEMO_SECRET").map(SecretValue::expose), Some("token-hunter2"));
    }
}
