//! The params an operation runs with: a fresh start's, checked against the manifest's and filled
//! with their defaults; an existing run's, its recorded ones replayed.

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, SetBy};

use crate::scope::Scope;
use crate::{Error, Result};

/// The params of a fresh start in `scope`: `given` checked against the manifest's, defaults filled
/// in; a param that doesn't concern the scope (an ESXi host on VirtualBox) is left out.
pub(crate) fn params_for_start(manifest: &Manifest, given: &BTreeMap<String, String>, scope: &Scope) -> Result<BTreeMap<String, String>> {
    let refuse = |name: &str, reason: String| Error::Param { name: name.to_owned(), reason };
    if let Some(name) = given.keys().find(|n| !manifest.params.iter().any(|p| p.name.as_str() == n.as_str())) {
        return Err(refuse(name, "isn't a param of this manifest".into()));
    }
    let mut out = BTreeMap::new();
    for param in manifest.params.iter().filter(|p| scope.admits(&p.hosts, &p.providers)) {
        let name = param.name.as_str();
        let value = match given.get(name) {
            Some(_) if param.set_by == SetBy::Runner => {
                return Err(refuse(name, "is set by the runner, not the embedder".into()));
            }
            Some(v) => Some(v.clone()),
            None => param.default.clone(),
        };
        match value {
            Some(v) if !param.values.is_empty() && !param.values.contains(&v) => {
                return Err(refuse(name, format!("`{v}` isn't one of {}", param.values.join(", "))));
            }
            Some(v) => {
                out.insert(name.to_owned(), v);
            }
            None if param.required => {
                return Err(refuse(name, "is required and has no value".into()));
            }
            None => {}
        }
    }
    Ok(out)
}

/// The params of an operation on a run that exists: the recorded ones win, so a `down` acts on
/// what was created (the region it was applied in), not on what the embedder's settings say now;
/// a given param the record lacks is added.
pub(crate) fn params_for_existing(
    manifest: &Manifest,
    recorded: Option<&BTreeMap<String, String>>,
    given: &BTreeMap<String, String>,
    scope: &Scope,
) -> Result<BTreeMap<String, String>> {
    let Some(recorded) = recorded else {
        return params_for_start(manifest, given, scope);
    };
    let mut merged = given.clone();
    merged.extend(recorded.iter().map(|(k, v)| (k.clone(), v.clone())));
    merged.retain(|k, _| manifest.params.iter().any(|p| p.name.as_str() == k));
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use isoloom_manifest::HostOs;

    use super::*;
    use crate::testing::docker_manifest;

    fn scope() -> Scope {
        Scope {
            host: HostOs::Linux,
            provider: None,
        }
    }

    fn given(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }
    #[test]
    fn a_start_fills_defaults_and_checks_values() {
        let m = docker_manifest();
        let params = params_for_start(&m, &given(&[("ISOLOOM_PUBLISH_FIXED", "1")]), &scope()).unwrap();
        assert_eq!(
            params,
            given(&[
                ("ISOLOOM_DERIVED", "1"),
                ("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1"),
                ("ISOLOOM_PUBLISH_FIXED", "1")
            ])
        );
        let bad = params_for_start(&m, &given(&[("ISOLOOM_DERIVED", "2")]), &scope()).unwrap_err();
        assert_eq!(bad.to_string(), "param `ISOLOOM_DERIVED`: `2` isn't one of 0, 1");
        assert!(
            params_for_start(&m, &given(&[("REGION", "x")]), &scope())
                .unwrap_err()
                .to_string()
                .contains("isn't a param")
        );
    }

    #[test]
    fn an_existing_run_replays_its_recorded_params() {
        // Regression: a destroy used the profile's current settings, not the applied ones.
        let m = docker_manifest();
        let recorded = given(&[("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1")]);
        let now = given(&[("ISOLOOM_PUBLISH_ADDRESS", "0.0.0.0"), ("ISOLOOM_DERIVED", "0")]);
        let params = params_for_existing(&m, Some(&recorded), &now, &scope()).unwrap();
        assert_eq!(params, given(&[("ISOLOOM_DERIVED", "0"), ("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1")]));
    }
}
