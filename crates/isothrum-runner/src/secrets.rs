//! Secret and input values: held by the embedder, handed in per operation, never printed, and
//! stored only where a delivery says so (a `state_file`, owner-only in the run's state folder
//! until a successful `down`; a `guest` delivery's input, owner-only there while it is
//! delivered). A value reaches only the steps its slot delivers it to, and is masked in every
//! event.
//! A required slot without a value refuses an operation that needs it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use isoloom_manifest::Manifest;

use crate::scope::Scope;
use crate::{Error, Result};

/// A secret's value. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Holds `value`.
    pub fn new(value: impl Into<String>) -> SecretValue {
        SecretValue(value.into())
    }

    /// The value itself, for the step it is delivered to.
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

/// The values of a run's secret and input slots, by slot name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Secrets(BTreeMap<String, SecretValue>);

impl Secrets {
    /// No values.
    #[must_use]
    pub fn new() -> Secrets {
        Secrets::default()
    }

    /// Sets slot `name`'s value.
    pub fn insert(&mut self, name: impl Into<String>, value: SecretValue) {
        self.0.insert(name.into(), value);
    }

    /// Slot `name`'s value, when given.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&SecretValue> {
        self.0.get(name)
    }

    /// Every slot given a value, by name.
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

/// Fails when a required secret slot of `scope` has no value.
pub(crate) fn check_secrets(manifest: &Manifest, secrets: &Secrets, scope: &Scope) -> Result<()> {
    let missing = |s: &&isoloom_manifest::SecretSlot| s.required && scope.admits(&BTreeSet::new(), &s.providers) && secrets.get(s.name.as_str()).is_none();
    match manifest.secrets.iter().find(missing) {
        Some(slot) => Err(Error::SecretMissing { name: slot.name.to_string() }),
        None => Ok(()),
    }
}

/// The values of the sensitive slots given, to mask.
pub(crate) fn sensitive_values(manifest: &Manifest, secrets: &Secrets) -> Vec<String> {
    manifest
        .secrets
        .iter()
        .filter(|s| s.sensitive)
        .filter_map(|s| secrets.get(s.name.as_str()))
        .map(|v| v.expose().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::docker_manifest;

    #[test]
    fn a_secret_never_prints() {
        let mut s = Secrets::new();
        s.insert("DEMO_SECRET", SecretValue::new("token-hunter2"));
        let shown = format!("{s:?}");
        assert!(!shown.contains("hunter2") && shown.contains("<redacted>"), "{shown}");
        assert_eq!(s.get("DEMO_SECRET").map(SecretValue::expose), Some("token-hunter2"));
    }

    fn scope() -> Scope {
        Scope {
            host: isoloom_manifest::HostOs::Linux,
            provider: None,
        }
    }

    #[test]
    fn a_required_secret_must_be_given_and_sensitive_ones_are_masked() {
        let mut m = docker_manifest();
        m.secrets[0].required = true;
        assert_eq!(
            check_secrets(&m, &Secrets::new(), &scope()).unwrap_err().code(),
            crate::ErrorCode::SecretMissing
        );
        let mut s = Secrets::new();
        s.insert("DEMO_SECRET", SecretValue::new("token-x"));
        check_secrets(&m, &s, &scope()).unwrap();
        assert_eq!(sensitive_values(&m, &s), ["token-x"]);
    }
}
