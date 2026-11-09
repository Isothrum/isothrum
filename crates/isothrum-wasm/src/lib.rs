//! The editor's engine, compiled to WebAssembly: a topology (the editor's JSON) becomes an
//! `isoloom.yml` in Isoloom's format, the files every Isoloom target generates from it, and one
//! zip to download. Nothing leaves the browser: Isoloom itself produces the Docker, Vagrant and
//! Terraform files. The same build runs in a server's JavaScript runtime too.
//!
//! The functions JavaScript calls are the `#[wasm_bindgen]` ones ([`plan_json`], [`bundle`],
//! [`bundle_target`], [`own_files_json`], and [`choices_json`], Isoloom's own lists for the
//! editor's tests); the Rust functions under them are the same API for Rust callers and tests.
//!
//! ```
//! let topology = r#"{
//!   "name": "pair",
//!   "networks": { "office": { "cidr": "10.10.1.0/24" } },
//!   "nodes": {
//!     "web":  { "kind": { "container": { "image": "nginx:1.27-alpine" } }, "networks": { "office": null } },
//!     "user": { "kind": { "vm": { "os": "debian@12" } }, "access": true, "networks": { "office": null } }
//!   }
//! }"#;
//! let plan = isothrum_wasm::plan(topology);
//! assert!(plan.ok && plan.problems.is_empty(), "{:?}", plan.problems);
//! assert!(plan.files.iter().any(|f| f.path == ".isoloom/docker/compose.yml"));
//! assert_eq!(plan.addresses["web"], [("office".to_owned(), "10.10.1.10".to_owned())]);
//! ```
//!
//! The modules: `plan` (what the editor shows), `bundle` (the zips to download), `files` (the
//! files handed out, the project's own among them), `choices` (Isoloom's lists), `js` (the JavaScript
//! functions) and `error`.

mod bundle;
mod choices;
mod error;
mod files;
mod js;
mod plan;

pub use bundle::{bundle_bytes, bundle_target_bytes};
pub use choices::{Choices, choices};
pub use error::{Error, Result};
pub use files::{File, own_files};
pub use js::{bundle, bundle_target, choices_json, own_files_json, plan_json};
pub use plan::{Plan, plan};
