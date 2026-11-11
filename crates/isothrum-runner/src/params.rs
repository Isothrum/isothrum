//! The params an operation runs with: a fresh start's, checked against the manifest's and filled
//! with their defaults; an existing run's, its recorded ones replayed, then filled the same way.

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, Param, SetBy};

use crate::scope::Scope;
use crate::{Error, Result};

/// The params of a fresh start in `scope`: `given` checked against the manifest's, defaults filled
/// in; a param that doesn't concern the scope (an ESXi host on VirtualBox) is left out.
pub(crate) fn params_for_start(manifest: &Manifest, given: &BTreeMap<String, String>, scope: &Scope) -> Result<BTreeMap<String, String>> {
    check_given(manifest, given)?;
    filled(manifest, scope, |param| {
        let value = given.get(param.name.as_str()).or(param.default.as_ref());
        if let Some(v) = value {
            check_value(param, v)?;
        }
        Ok(value.cloned())
    })
}

/// The params of an operation on a run that exists: `given` checked as a start checks it, then the
/// recorded ones win, so a `down` acts on what was created (the region it was applied in), not on
/// what the embedder's settings say now; a given param the record lacks is added, and one neither
/// gives takes its default (a run taken over records only the params it was given; a manifest
/// regenerated since may add one). As at a start, a param that doesn't concern the scope is left
/// out and a required one with no value is refused.
pub(crate) fn params_for_existing(
    manifest: &Manifest,
    recorded: Option<&BTreeMap<String, String>>,
    given: &BTreeMap<String, String>,
    scope: &Scope,
) -> Result<BTreeMap<String, String>> {
    let Some(recorded) = recorded else {
        return params_for_start(manifest, given, scope);
    };
    check_given(manifest, given)?;
    filled(manifest, scope, |param| {
        let name = param.name.as_str();
        Ok(recorded.get(name).or_else(|| given.get(name)).or(param.default.as_ref()).cloned())
    })
}

/// Each param of `manifest` that concerns `scope`, valued by `value`; a required one it gives no
/// value is refused.
fn filled(manifest: &Manifest, scope: &Scope, value: impl Fn(&Param) -> Result<Option<String>>) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for param in manifest.params.iter().filter(|p| scope.admits(&p.hosts, &p.providers)) {
        let name = param.name.as_str();
        match value(param)? {
            Some(v) => {
                out.insert(name.to_owned(), v);
            }
            None if param.required => return Err(refuse(name, "is required and has no value".into())),
            None => {}
        }
    }
    Ok(out)
}

/// Refuses a given param the manifest doesn't take, one the runner sets, or a value outside the
/// param's `values`: the one check of what an embedder passes, for a start and an existing run.
fn check_given(manifest: &Manifest, given: &BTreeMap<String, String>) -> Result<()> {
    for (name, value) in given {
        let Some(param) = manifest.params.iter().find(|p| p.name.as_str() == name.as_str()) else {
            return Err(refuse(name, "isn't a param of this manifest".into()));
        };
        if param.set_by == SetBy::Runner {
            return Err(refuse(name, "is set by the runner, not the embedder".into()));
        }
        check_value(param, value)?;
    }
    Ok(())
}

/// Refuses `value` for `param` when the param lists its `values` and `value` isn't one of them.
fn check_value(param: &Param, value: &str) -> Result<()> {
    if param.values.is_empty() || param.values.iter().any(|v| v == value) {
        return Ok(());
    }
    Err(refuse(param.name.as_str(), format!("`{value}` isn't one of {}", param.values.join(", "))))
}

/// The error refusing param `name` for `reason`.
fn refuse(name: &str, reason: String) -> Error {
    Error::Param { name: name.to_owned(), reason }
}

#[cfg(test)]
mod tests {
    use isoloom_manifest::HostOs;

    use super::*;
    use crate::testing::docker_manifest;

    fn scope() -> Scope {
        Scope::new(HostOs::Linux, None)
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

    #[test]
    fn an_existing_run_checks_the_given_params_as_a_start_does() {
        // Regression: a mistyped `--param` on `down` was dropped and a bad value passed unchecked.
        let m = docker_manifest();
        let recorded = given(&[("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1")]);
        let unknown = params_for_existing(&m, Some(&recorded), &given(&[("REGION", "x")]), &scope()).unwrap_err();
        assert_eq!(unknown.to_string(), "param `REGION`: isn't a param of this manifest");
        let bad = params_for_existing(&m, Some(&recorded), &given(&[("ISOLOOM_DERIVED", "2")]), &scope()).unwrap_err();
        assert_eq!(bad.to_string(), "param `ISOLOOM_DERIVED`: `2` isn't one of 0, 1");
    }

    /// `m` with one more param: `name`, its `default`, `required` or not, for VirtualBox only when
    /// `virtualbox`.
    fn with_param(mut m: Manifest, name: &str, default: Option<&str>, required: bool, virtualbox: bool) -> Manifest {
        let mut p = m.params[0].clone();
        p.name = name.parse().unwrap();
        p.default = default.map(str::to_owned);
        p.values = Vec::new();
        p.required = required;
        p.set_by = SetBy::Embedder;
        p.providers = if virtualbox {
            [isoloom_manifest::Provider::Virtualbox].into()
        } else {
            std::collections::BTreeSet::new()
        };
        m.params.push(p);
        m
    }

    #[test]
    fn an_existing_run_gets_the_default_of_a_param_its_record_lacks() {
        // Regression: a run taken over, or a manifest regenerated with a new param, left the param
        // unbound, while a secret slot fell back to its default.
        let m = with_param(docker_manifest(), "REGION", Some("eu-west-3"), false, false);
        let recorded = given(&[("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1")]);
        let params = params_for_existing(&m, Some(&recorded), &BTreeMap::new(), &scope()).unwrap();
        assert_eq!(params.get("REGION").map(String::as_str), Some("eu-west-3"));
        assert_eq!(params.get("ISOLOOM_DERIVED").map(String::as_str), Some("1"), "the manifest's own default");
        let given_one = params_for_existing(&m, Some(&recorded), &given(&[("REGION", "us-east-1")]), &scope()).unwrap();
        assert_eq!(given_one.get("REGION").map(String::as_str), Some("us-east-1"));
    }

    #[test]
    fn an_existing_run_refuses_a_required_param_with_no_value_and_leaves_out_another_scopes() {
        let m = with_param(docker_manifest(), "REGION", None, true, false);
        let recorded = given(&[("ISOLOOM_PUBLISH_ADDRESS", "127.0.0.1")]);
        let err = params_for_existing(&m, Some(&recorded), &BTreeMap::new(), &scope()).unwrap_err();
        assert_eq!(err.to_string(), "param `REGION`: is required and has no value");
        let vbox = with_param(docker_manifest(), "VBOX_ONLY", Some("x"), false, true);
        let given_vbox = given(&[("VBOX_ONLY", "y")]);
        let params = params_for_existing(&vbox, Some(&recorded), &given_vbox, &scope()).unwrap();
        assert!(!params.contains_key("VBOX_ONLY"), "{params:?}");
    }
}
