//! What a run gives back: its outputs (the entry URL from a published port, no guessing at which
//! container looks like a web UI), and the run's message filled from them.

use std::collections::{BTreeMap, BTreeSet};

use isoloom_manifest::{Manifest, OutputSource};

use crate::events::{Emitter, EventKind};
use crate::status::OutputValue;
use crate::vars::Vars;

/// The manifest's outputs with the values `vars` (the published ports) and `engine` (a Terraform
/// state's outputs) give; an output that can't be read yet has none. The outputs without a
/// `format` are read first, so an entry's format can name one (`http://${outputs.ip}:${value}/`).
/// An output is sensitive when the manifest says so, or when it comes from an engine output
/// `engine_sensitive` names (the module marks it: a run's own module Isoloom can't read).
pub(crate) fn read(manifest: &Manifest, vars: &Vars, engine: &BTreeMap<String, String>, engine_sensitive: &BTreeSet<String>) -> Vec<OutputValue> {
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
    manifest
        .outputs
        .iter()
        .zip(raw)
        .map(|(o, raw)| {
            let at = format!("outputs.{}.format", o.name);
            let value = raw.and_then(|v| match &o.format {
                Some(format) => known.render_value(format, &v, &at).ok(),
                None => Some(v),
            });
            OutputValue::new(o.name.to_string(), o.role, sensitive(o), value)
        })
        .collect()
}

/// Binds `outputs` into `vars` (`${outputs.<name>}`), sensitive ones excepted.
pub(crate) fn bind(vars: &mut Vars, outputs: &[OutputValue]) {
    for o in outputs.iter().filter(|o| !o.sensitive) {
        if let Some(v) = &o.value {
            vars.set_output(&o.name, v.clone());
        }
    }
}

/// The run's message, filled; a sensitive output (as the manifest or the engine says, in
/// `outputs`), or a value not known, stays as written.
pub(crate) fn message(manifest: &Manifest, vars: &Vars, outputs: &[OutputValue]) -> Option<String> {
    let marked = outputs.iter().filter(|o| o.sensitive).map(|o| o.name.clone());
    let hidden: BTreeSet<String> = manifest
        .outputs
        .iter()
        .filter(|o| o.sensitive)
        .map(|o| o.name.to_string())
        .chain(marked)
        .collect();
    manifest.environment.message.as_ref().map(|m| vars.render_lenient(m, &hidden))
}

/// Emits each output (a sensitive value as `***`, as [`OutputValue`] holds it).
pub(crate) fn emit(events: &Emitter<'_>, outputs: &[OutputValue]) {
    for o in outputs {
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
        assert_eq!(read(&m, &vars, &none, &marked)[0].value, None, "not published yet");
        vars.set_published(BTreeMap::from([(("web".to_owned(), "portal".to_owned()), 41873)]));
        let outputs = read(&m, &vars, &none, &marked);
        assert_eq!(
            (outputs[0].name.as_str(), outputs[0].value.as_deref()),
            ("web_portal", Some("http://127.0.0.1:41873/"))
        );
        bind(&mut vars, &outputs);
        assert_eq!(message(&m, &vars, &outputs).unwrap(), "The portal answers at http://localhost:41873/.\n");
    }
}
