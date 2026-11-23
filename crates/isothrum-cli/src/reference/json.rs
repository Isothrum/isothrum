//! The `--json` output as a stable interface, written from the types that print it: one JSON
//! Schema of every document the command line prints with `--json`
//! (`docs/reference/json-output.schema.json`), and the tables of `docs/reference/events.md` that
//! come from it (the common fields, the event kinds, the error details), between its markers. The
//! prose around them is the page's own.

use std::fmt::Write as _;

use isothrum_runner::{ShellCommand, events_schema, status_schema};
use serde_json::{Map, Value, json};

use super::{check_or_write, page};
use crate::documents::{NativeApps, PublicKey, Screen};

/// The schema's file, under `docs/reference/`.
const SCHEMA: &str = "json-output.schema.json";

/// Each document, by the name the schema gives it, with what prints it and its own schema.
fn documents() -> [(&'static str, &'static str, Value); 6] {
    let of = |schema: schemars::Schema| serde_json::to_value(schema).unwrap();
    [
        (
            "Event",
            "Each line of an operation (`up`, `down`, `park`, `resume`, `update`, `retire`, `exec`, `provision`, `attach`, `reap`), of `events`, and any failure",
            events_schema(),
        ),
        ("Status", "`status`", status_schema()),
        ("ShellCommand", "`console`", of(schemars::schema_for!(ShellCommand))),
        ("Screen", "`screen`", of(schemars::schema_for!(Screen))),
        ("NativeApps", "`screen --app`", of(schemars::schema_for!(NativeApps))),
        ("PublicKey", "`key`", of(schemars::schema_for!(PublicKey))),
    ]
}

/// The schema of the `--json` output: one line is one of the documents, each under `$defs` with
/// the types they share.
fn schema() -> Value {
    let mut defs = Map::new();
    let mut which = Vec::new();
    let mut said = String::from(
        "What `isothrum <command> --json` prints on standard output, one JSON document per line. Read one line at a time; ignore a field or an event kind you don't know.\n",
    );
    for (name, printed_by, document) in documents() {
        let Value::Object(mut document) = document else {
            panic!("{name}: a schema that isn't an object")
        };
        for (def, body) in document.remove("$defs").and_then(|d| d.as_object().cloned()).unwrap_or_default() {
            if let Some(other) = defs.insert(def.clone(), body.clone()) {
                assert_eq!(other, body, "two documents define `{def}` differently");
            }
        }
        document.remove("$schema");
        document.remove("title");
        assert!(
            defs.insert(name.to_owned(), Value::Object(document)).is_none(),
            "`{name}` is also a shared type's name"
        );
        which.push(json!({ "$ref": format!("#/$defs/{name}") }));
        writeln!(said, "\n- `{name}`: {printed_by}.").unwrap();
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "isothrum --json output",
        "description": said,
        "anyOf": which,
        "$defs": defs,
    })
}

/// A description as one Markdown table cell: its first paragraph on one line, rustdoc's links
/// reduced to their text, `|` escaped.
fn cell(text: &str) -> String {
    let first = text.split("\n\n").next().unwrap_or_default();
    first
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("[`", "`")
        .replace("`]", "`")
        .replace('|', "\\|")
}

/// `value[key]`, or null.
fn at<'v>(value: &'v Value, key: &str) -> &'v Value {
    value.get(key).unwrap_or(&Value::Null)
}

/// The fields of the object schema `object` but `skip`, each `name` or, when optional, `name?`,
/// in the order the schema lists them.
fn fields(object: &Value, skip: &str) -> String {
    let required: Vec<&Value> = at(object, "required").as_array().map(|r| r.iter().collect()).unwrap_or_default();
    let names = at(object, "properties").as_object().into_iter().flat_map(|p| p.keys()).filter(|k| *k != skip);
    let shown: Vec<String> = names
        .map(|k| if required.contains(&&json!(k)) { format!("`{k}`") } else { format!("`{k}`?") })
        .collect();
    shown.join(", ")
}

/// The tables of `events.md`, from the event schema `events` and its definitions `defs`.
fn tables(events: &Value, defs: &Value) -> String {
    let mut out = String::from("### Common fields\n\n| Field | Meaning |\n|---|---|\n");
    for (name, field) in at(events, "properties").as_object().into_iter().flatten() {
        let optional = if at(events, "required").as_array().is_some_and(|r| r.contains(&json!(name))) {
            ""
        } else {
            " Optional."
        };
        writeln!(out, "| `{name}` | {}{optional} |", cell(at(field, "description").as_str().unwrap_or_default())).unwrap();
    }
    out.push_str("\n### Kinds\n\n| `event` | Fields | Meaning |\n|---|---|---|\n");
    for kind in at(events, "oneOf").as_array().into_iter().flatten() {
        let name = at(at(at(kind, "properties"), "event"), "const").as_str().unwrap_or_default();
        // `error` is its report's fields, flattened.
        let report = at(kind, "$ref").as_str().and_then(|r| r.strip_prefix("#/$defs/")).map(|d| at(defs, d));
        let own = fields(kind, "event");
        let all = [own, report.map(|r| fields(r, "")).unwrap_or_default()]
            .into_iter()
            .filter(|f| !f.is_empty())
            .collect::<Vec<_>>();
        writeln!(
            out,
            "| `{name}` | {} | {} |",
            all.join(", "),
            cell(at(kind, "description").as_str().unwrap_or_default())
        )
        .unwrap();
    }
    out.push_str("\n### Error details\n\nAn `error`'s `detail`, when the failure has typed fields: `v`, its `kind`, and the kind's own fields.\n\n| `kind` | Fields | Meaning |\n|---|---|---|\n");
    for detail in at(at(defs, "ErrorDetail"), "oneOf").as_array().into_iter().flatten() {
        let kind = at(at(at(detail, "properties"), "kind"), "const").as_str().unwrap_or_default();
        writeln!(
            out,
            "| `{kind}` | {} | {} |",
            fields(detail, "kind"),
            cell(at(detail, "description").as_str().unwrap_or_default())
        )
        .unwrap();
    }
    out
}

/// The line before the generated tables of `events.md`, and the one after.
const BEGIN: &str =
    "<!-- Written from the schema: don't edit down to the end marker. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->";
const END: &str = "<!-- End of what is written from the schema. -->";

/// `events.md` as committed, its tables written again from `schema`.
fn events_page(schema: &Value) -> String {
    let committed = std::fs::read_to_string(page("events.md")).unwrap();
    let (before, rest) = committed.split_once(BEGIN).expect("events.md has its begin marker");
    let (_, after) = rest.split_once(END).expect("events.md has its end marker");
    let defs = at(schema, "$defs");
    format!("{before}{BEGIN}\n\n{}\n{END}{after}", tables(at(defs, "Event"), defs))
}

#[test]
fn the_committed_json_output_schema_is_current() {
    check_or_write(SCHEMA, &(serde_json::to_string_pretty(&schema()).unwrap() + "\n"));
}

#[test]
fn the_committed_event_tables_are_current() {
    check_or_write("events.md", &events_page(&schema()));
}
