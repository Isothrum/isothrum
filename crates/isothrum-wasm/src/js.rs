//! The functions JavaScript calls (`#[wasm_bindgen]`): the Rust API, with its values as JSON and
//! its errors as JavaScript `Error`s whose `message` is the error's.

use wasm_bindgen::prelude::*;

use crate::{bundle_bytes, bundle_target_bytes, choices, own_files, plan};

/// The download for one target (see [`bundle_target_bytes`]).
///
/// # Errors
///
/// The [`Error`](crate::Error)'s message, as a JavaScript `Error`.
#[wasm_bindgen]
pub fn bundle_target(topology_json: &str, target_dir: &str) -> Result<Vec<u8>, JsError> {
    bundle_target_bytes(topology_json, target_dir).map_err(|e| JsError::new(&e.to_string()))
}

/// The download: a zip with `isoloom.yml` and every generated file (see [`bundle_bytes`]).
///
/// # Errors
///
/// The [`Error`](crate::Error)'s message, as a JavaScript `Error`.
#[wasm_bindgen]
pub fn bundle(topology_json: &str) -> Result<Vec<u8>, JsError> {
    bundle_bytes(topology_json).map_err(|e| JsError::new(&e.to_string()))
}

/// JSON for a JavaScript caller: [`own_files`].
///
/// # Errors
///
/// A JavaScript `Error` if the JSON couldn't be written, which its values (strings, lists and
/// maps of strings) never cause.
#[wasm_bindgen]
pub fn own_files_json(topology_json: &str) -> Result<String, JsError> {
    json(&own_files(topology_json))
}

/// JSON for the editor: a [`Plan`](crate::Plan), one that says why it has nothing when the
/// topology gives none.
///
/// # Errors
///
/// A JavaScript `Error` if the JSON couldn't be written, which its values (strings, lists and
/// maps of strings) never cause.
#[wasm_bindgen]
pub fn plan_json(topology_json: &str) -> Result<String, JsError> {
    json(&plan(topology_json))
}

/// JSON for the editor: Isoloom's lists ([`Choices`](crate::Choices)), what its OS, appliance and
/// protocol fields offer.
///
/// # Errors
///
/// A JavaScript `Error` if the JSON couldn't be written, which its values (strings, lists and
/// maps of strings) never cause.
#[wasm_bindgen]
pub fn choices_json() -> Result<String, JsError> {
    json(&choices())
}

/// `value` as JSON. Every value handed out is strings, lists and maps of strings, so writing it
/// can't fail; were it to, the caller gets a JavaScript `Error` saying why, not a stand-in value.
fn json(value: &impl serde::Serialize) -> Result<String, JsError> {
    serde_json::to_string(value).map_err(|e| JsError::new(&format!("writing the JSON: {e}")))
}
