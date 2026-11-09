//! The functions JavaScript calls (`#[wasm_bindgen]`): the Rust API, with its values as JSON and
//! its errors as JavaScript `Error`s whose `message` is the error's.

use wasm_bindgen::prelude::*;

use crate::{Plan, bundle_bytes, bundle_target_bytes, choices, own_files, plan};

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
#[wasm_bindgen]
#[must_use]
pub fn own_files_json(topology_json: &str) -> String {
    serde_json::to_string(&own_files(topology_json)).unwrap_or_else(|_| "[]".into())
}

/// JSON for the editor: a [`Plan`](crate::Plan).
#[wasm_bindgen]
#[must_use]
pub fn plan_json(topology_json: &str) -> String {
    // A plan is strings, lists and maps of strings: writing it can't fail. Were it to, the editor
    // still gets a plan, one that says why it has nothing.
    serde_json::to_string(&plan(topology_json))
        .or_else(|e| serde_json::to_string(&Plan::failed(&format!("writing the plan: {e}"))))
        .unwrap_or_default()
}

/// JSON for the editor: Isoloom's lists ([`Choices`](crate::Choices)), what its OS, appliance and
/// protocol fields offer.
#[wasm_bindgen]
#[must_use]
pub fn choices_json() -> String {
    serde_json::to_string(&choices()).unwrap_or_else(|_| "{}".into())
}
