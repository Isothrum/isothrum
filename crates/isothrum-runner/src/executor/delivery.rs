//! Delivering params and secrets: each slot's value reaches exactly the steps its `env` delivery
//! names (`*`: every step), as an environment variable, never on a command line. A slot this
//! operation has no value for gets its manifest `default` when it has one (Isoloom gives every
//! docker input slot `default: ""`: Compose refuses an unset variable for a secret it reads from
//! the environment, on every start, and keeps what a container was created with when it is
//! empty), and isn't delivered otherwise. A slot for another provider has no value here, wherever
//! it goes ([`slot_value`]: the guest delivery reads it too).

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{Delivery, NonEmpty, SecretSlot, StepSelection, TfVarShape};

use super::Cx;
use crate::secrets::SecretValue;

/// The variables step `id` gets from the manifest's params and secret slots: those that concern
/// this host and provider (an ESXi password never reaches a VirtualBox run). A slot without a
/// value gets its `default`, else isn't delivered. Terraform variables reach every step.
pub(crate) fn step_env(cx: &Cx<'_>, id: &str) -> Vec<(String, String)> {
    let valued = valued(cx);
    let mut env = delivered(&valued, id);
    env.extend(tf_vars(&valued));
    env
}

/// The variables the manifest delivers to step `id` by name (`env` deliveries), without the
/// Terraform variables every step gets: what an `ssh.run` step carries into its guest.
pub(crate) fn env_deliveries(cx: &Cx<'_>, id: &str) -> Vec<(String, String)> {
    delivered(&valued(cx), id)
}

/// The `env` deliveries of `valued` that name step `id`.
fn delivered(valued: &[(&str, String, &NonEmpty<Delivery>)], id: &str) -> Vec<(String, String)> {
    valued
        .iter()
        .flat_map(|(_, value, deliveries)| env_vars(deliveries.iter(), id).map(move |var| (var, value.clone())))
        .collect()
}

/// Every delivery of `manifest`'s params, then of its slots.
pub(crate) fn every_delivery(manifest: &isoloom_manifest::Manifest) -> impl Iterator<Item = &Delivery> {
    let params = manifest.params.iter().flat_map(|p| p.deliver.iter());
    params.chain(manifest.secrets.iter().flat_map(|s| s.deliver.iter()))
}

/// The value slot `slot` has in this operation, wherever it goes: the one given, else its
/// `default`; none when it has neither, or when the slot doesn't concern this operation (one for
/// another provider).
pub(crate) fn slot_value<'a>(cx: &'a Cx<'_>, slot: &'a SecretSlot) -> Option<&'a str> {
    if !cx.scope.admits(&BTreeSet::new(), &slot.providers) {
        return None;
    }
    cx.secrets.get(slot.name.as_str()).map(SecretValue::expose).or(slot.default.as_deref())
}

/// The params and slots this operation has a value for ([`slot_value`]), with where each goes.
pub(super) fn valued<'a>(cx: &'a Cx<'_>) -> Vec<(&'a str, String, &'a NonEmpty<Delivery>)> {
    let manifest = cx.run.manifest();
    let params = manifest
        .params
        .iter()
        .filter(|p| cx.scope.admits(&p.hosts, &p.providers))
        .filter_map(|p| Some((p.name.as_str(), cx.vars.params().get(p.name.as_str())?.clone(), &p.deliver)));
    let slots = manifest
        .secrets
        .iter()
        .filter_map(|s| Some((s.name.as_str(), slot_value(cx, s)?.to_owned(), &s.deliver)));
    params.chain(slots).collect()
}

/// The Terraform variables of `valued` (`TF_VAR_<var>`): a `string` one is the value; the
/// `map_entry` ones of a variable make one JSON object, each value under its slot's name (the
/// module's `inputs`).
fn tf_vars(valued: &[(&str, String, &NonEmpty<Delivery>)]) -> Vec<(String, String)> {
    let mut plain: Vec<(String, String)> = Vec::new();
    let mut maps: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for (name, value, deliveries) in valued {
        for d in *deliveries {
            match d {
                Delivery::TfVar {
                    var,
                    shape: TfVarShape::String,
                } => plain.push((format!("TF_VAR_{var}"), value.clone())),
                Delivery::TfVar {
                    var,
                    shape: TfVarShape::MapEntry,
                } => {
                    maps.entry(var.to_string())
                        .or_default()
                        .insert((*name).to_owned(), serde_json::Value::String(value.clone()));
                }
                _ => {}
            }
        }
    }
    let objects = maps
        .into_iter()
        .map(|(var, o)| (format!("TF_VAR_{var}"), serde_json::Value::Object(o).to_string()));
    plain.into_iter().chain(objects).collect()
}

/// The variables of `deliveries` that reach step `id`.
fn env_vars<'a>(deliveries: impl Iterator<Item = &'a Delivery> + 'a, id: &'a str) -> impl Iterator<Item = String> + 'a {
    deliveries.filter_map(move |d| match d {
        Delivery::Env { var, steps } if reaches(steps, id) => Some(var.to_string()),
        _ => None,
    })
}

/// Whether `steps` names step `id`.
pub(crate) fn reaches(steps: &StepSelection, id: &str) -> bool {
    match steps {
        StepSelection::Every => true,
        StepSelection::Only(ids) => ids.iter().any(|s| s.as_str() == id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deliveries(json: &str) -> NonEmpty<Delivery> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_string_variable_is_its_value_and_the_map_entries_of_one_make_one_json_object() {
        let token = deliveries(r#"[{"via": "tf_var", "var": "api_token"}]"#);
        let secret = deliveries(r#"[{"via": "tf_var", "var": "inputs", "shape": "map_entry"}]"#);
        let user = deliveries(r#"[{"via": "tf_var", "var": "inputs", "shape": "map_entry"}, {"via": "env", "var": "USER_NAME", "steps": ["*"]}]"#);
        let valued = [
            ("token", "t0k".to_owned(), &token),
            ("DEMO_SECRET", "secret-x".to_owned(), &secret),
            ("user", "ada".to_owned(), &user),
        ];
        assert_eq!(
            tf_vars(&valued),
            [
                ("TF_VAR_api_token".to_owned(), "t0k".to_owned()),
                ("TF_VAR_inputs".to_owned(), r#"{"DEMO_SECRET":"secret-x","user":"ada"}"#.to_owned())
            ]
        );
    }

    #[test]
    fn a_variable_reaches_the_steps_it_names_or_every_one() {
        let every: StepSelection = serde_json::from_str(r#"["*"]"#).unwrap();
        assert!(reaches(&every, "up"));
        let only: StepSelection = serde_json::from_str(r#"["up"]"#).unwrap();
        assert!(reaches(&only, "up") && !reaches(&only, "down"));
    }
}
