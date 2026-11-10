//! What a run gives back: its outputs (the entry URL from a published port, no guessing at which
//! container looks like a web UI), and the run's message filled from them.

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{Manifest, OutputSource};

use crate::error::Error;
use crate::events::{Emitter, EventKind, WarningCode};
use crate::status::OutputValue;
use crate::vars::Vars;

/// A run's outputs as read.
pub(crate) struct Read {
    /// Each output of the manifest, in its order.
    pub(crate) values: Vec<OutputValue>,
    /// Why an output whose value was read has none: its `format` couldn't be filled.
    pub(crate) unformatted: Vec<Error>,
}

/// The manifest's outputs with the values `vars` (the published ports) and `engine` (a Terraform
/// state's outputs) give; an output that can't be read yet has none, as one whose `format` can't
/// be filled (its error in [`Read::unformatted`]). The outputs without a `format` are read first,
/// so an entry's format can name one (`http://${outputs.ip}:${value}/`). An output is sensitive
/// when the manifest says so, or when it comes from an engine output `engine_sensitive` names (the
/// module marks it: a run's own module Isoloom can't read).
pub(crate) fn read(manifest: &Manifest, vars: &Vars, engine: &BTreeMap<String, String>, engine_sensitive: &BTreeSet<String>) -> Read {
    let sensitive =
        |o: &isoloom_manifest::Output| o.sensitive || matches!(&o.from, OutputSource::TerraformOutput { name, .. } if engine_sensitive.contains(name.as_str()));
    let raw: Vec<Option<String>> = manifest
        .outputs
        .iter()
        .map(|o| match &o.from {
            OutputSource::PublishedPort { machine, port } => vars.published().get(&(machine.to_string(), port.to_string())).map(u16::to_string),
            OutputSource::TerraformOutput { name, default } => engine.get(name.as_str()).cloned().or_else(|| default.clone()),
        })
        .collect();
    let mut known = vars.clone();
    for (o, v) in manifest.outputs.iter().zip(&raw) {
        if let (None, false, Some(v)) = (&o.format, sensitive(o), v) {
            known.set_output(o.name.as_str(), v.clone());
        }
    }
    let mut unformatted = Vec::new();
    let values = manifest
        .outputs
        .iter()
        .zip(raw)
        .map(|(o, raw)| {
            let at = format!("outputs.{}.format", o.name);
            let value = raw.and_then(|v| match &o.format {
                Some(format) => known.render_value(format, &v, &at).map_err(|e| unformatted.push(e)).ok(),
                None => Some(v),
            });
            OutputValue::new(o.name.to_string(), o.role, sensitive(o), value)
        })
        .collect();
    Read { values, unformatted }
}

/// Binds `outputs` into `vars` (`${outputs.<name>}`), sensitive ones excepted.
pub(crate) fn bind(vars: &mut Vars, outputs: &[OutputValue]) {
    for o in outputs.iter().filter(|o| !o.sensitive) {
        if let Some(v) = &o.value {
            vars.set_output(&o.name, v.clone());
        }
    }
}

/// The run's message, filled; a sensitive output (as [`read`] marks it in `outputs`: the
/// manifest or the engine says so), or a value not known, stays as written.
pub(crate) fn message(manifest: &Manifest, vars: &Vars, outputs: &[OutputValue]) -> Option<String> {
    let hidden: BTreeSet<String> = outputs.iter().filter(|o| o.sensitive).map(|o| o.name.clone()).collect();
    manifest.environment.message.as_ref().map(|m| vars.render_lenient(m, &hidden))
}

/// Emits each output (a sensitive value as `***`, as [`OutputValue`] holds it), after a warning
/// for each output whose format couldn't be filled.
pub(crate) fn emit(events: &Emitter<'_>, read: &Read) {
    for e in &read.unformatted {
        events.warning(WarningCode::OutputUnformatted, &format!("an output has no value: {e}"));
    }
    for o in &read.values {
        events.emit(EventKind::Output {
            name: o.name.clone(),
            role: o.role,
            value: o.value.clone(),
            sensitive: o.sensitive,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::testing::docker_manifest;

    #[test]
    fn the_entry_comes_from_the_published_port_and_fills_the_message() {
        let m = docker_manifest();
        let mut vars = Vars::default();
        let none = BTreeMap::new();
        let marked = BTreeSet::new();
        assert_eq!(read(&m, &vars, &none, &marked).values[0].value, None, "not published yet");
        vars.set_published(BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        let Read { values: outputs, unformatted } = read(&m, &vars, &none, &marked);
        assert!(unformatted.is_empty(), "{unformatted:?}");
        assert_eq!(
            (outputs[0].name.as_str(), outputs[0].value.as_deref()),
            ("web__portal", Some("http://127.0.0.1:41873/"))
        );
        bind(&mut vars, &outputs);
        assert_eq!(message(&m, &vars, &outputs).unwrap(), "The portal answers at http://localhost:41873/.\n");
    }

    #[test]
    fn a_format_that_cant_be_filled_leaves_its_output_without_a_value_and_says_why() {
        let m = isoloom_manifest::parse(&crate::testing::unfillable_format()).unwrap();
        let mut vars = Vars::default();
        vars.set_published(BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        let read = read(&m, &vars, &BTreeMap::new(), &BTreeSet::new());
        assert_eq!((read.values[0].value.as_deref(), read.values[1].value.as_deref()), (None, Some("***")));
        let said: Vec<String> = read.unformatted.iter().map(ToString::to_string).collect();
        assert_eq!(said, ["outputs.web__portal.format: `${outputs.token}` has no value in this operation"]);
    }
}
