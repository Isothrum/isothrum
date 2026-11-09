//! The tests that keep the repository's crates to the standard, read from what `cargo metadata`
//! and the manifests say:
//!
//! - **one-way dependencies**: the libraries (the compiler and the runner) depend on no
//!   other crate of the repository and on no UI; the WASM build depends on the compiler alone
//!   (never the runner: Tokio's processes aren't WASM); the machine-setup domain builds on the
//!   runner, and the app (the command line) depends on the libraries and the domain, never the
//!   reverse; nothing depends on the xtask;
//! - **workspace hygiene**: every crate inherits the workspace's package fields, lints
//!   and dependencies, so each dependency is declared once, with one version.

use serde_json::Value;

use crate::repo;

/// What a crate is, for the dependency rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A library every app builds on: the compiler, the runner.
    Library,
    /// A library built on the base libraries that the apps build on: the machine-setup domain.
    Domain,
    /// The compiler's WebAssembly build.
    Binding,
    /// A program a person runs: the command line.
    App,
    /// The development tasks.
    Xtask,
}

/// The repository's crates and their roles.
const ROLES: [(&str, Role); 6] = [
    ("isothrum-compiler", Role::Library),
    ("isothrum-runner", Role::Library),
    // Builds on the runner and the apps build on it: a tier of its own between the base libraries
    // and the apps (out of this workspace, which consume it too).
    ("isothrum-machine", Role::Domain),
    ("isothrum-wasm", Role::Binding),
    ("isothrum-cli", Role::App),
    ("xtask", Role::Xtask),
];

/// Crates that make a user interface: a library never depends on one.
const UI: [&str; 7] = ["tauri", "tauri-build", "wasm-bindgen", "js-sys", "web-sys", "clap", "tracing-subscriber"];

/// What the compiler and its WASM build never depend on: they run in a browser.
const NOT_WASM: [&str; 2] = ["tokio", "process-wrap"];

fn role(name: &str) -> Option<Role> {
    ROLES.iter().find(|(n, _)| *n == name).map(|(_, r)| *r)
}

/// Whether crate `from` may depend on the repository's crate `to`.
fn allowed(from: &str, to: &str) -> bool {
    match (role(from), role(to)) {
        (Some(Role::Binding), _) => to == "isothrum-compiler",
        (Some(Role::App | Role::Xtask), Some(Role::Library | Role::Domain)) | (Some(Role::Domain), Some(Role::Library)) => true,
        _ => false,
    }
}

/// Every dependency of `from` on `to` (`kind` its kind, `None` for a normal one) that breaks the
/// rule, as a sentence.
fn problem(from: &str, to: &str, kind: Option<&str>) -> Option<String> {
    let normal = kind.is_none();
    let why = if role(to).is_some() && !allowed(from, to) {
        "dependencies go one way: the libraries, then the domain built on them, then the apps, never the reverse"
    } else if normal && role(from) == Some(Role::Library) && UI.contains(&to) {
        "a library never depends on a UI"
    } else if normal && matches!(from, "isothrum-compiler" | "isothrum-wasm") && NOT_WASM.contains(&to) {
        "the compiler and its WASM build run in a browser"
    } else {
        return None;
    };
    let kind = kind.map(|k| format!(" ({k})")).unwrap_or_default();
    Some(format!("{from} depends on {to}{kind}: {why}"))
}

/// Every edge that breaks the rule, from `cargo metadata --no-deps` output.
fn violations(metadata: &Value) -> Vec<String> {
    let mut edges: Vec<(String, String, Option<String>)> = Vec::new();
    for p in metadata["packages"].as_array().into_iter().flatten() {
        let from = p["name"].as_str().unwrap_or("").to_owned();
        for d in p["dependencies"].as_array().into_iter().flatten() {
            edges.push((from.clone(), d["name"].as_str().unwrap_or("").to_owned(), d["kind"].as_str().map(str::to_owned)));
        }
    }
    let mut found: Vec<String> = edges.iter().filter_map(|(from, to, kind)| problem(from, to, kind.as_deref())).collect();
    found.extend(
        metadata["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["name"].as_str())
            .filter(|n| role(n).is_none())
            .map(|n| format!("{n}: a crate the dependency rule doesn't place yet (xtask/src/workspace.rs)")),
    );
    found
}

/// What in a member's manifest doesn't come from the workspace: a package field, the lints, or
/// a dependency declared with its own version.
fn not_inherited(name: &str, manifest: &toml::Table) -> Vec<String> {
    let inherited = |v: Option<&toml::Value>| v.and_then(|v| v.get("workspace")).and_then(toml::Value::as_bool) == Some(true);
    let package = manifest.get("package");
    // `publish` is deliberately not here: publishability varies per crate (the library crates go
    // to crates.io, xtask and the fuzz targets do not), so it is set per package, not inherited.
    let mut found: Vec<String> = ["version", "edition", "rust-version", "license", "repository"]
        .into_iter()
        .filter(|field| !inherited(package.and_then(|p| p.get(*field))))
        .map(|field| format!("{name}: package.{field} isn't `{field}.workspace = true`"))
        .collect();
    if !inherited(manifest.get("lints")) {
        found.push(format!("{name}: [lints] isn't `workspace = true`"));
    }
    let mut tables: Vec<(String, &toml::Value)> = Vec::new();
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        tables.extend(manifest.get(key).map(|t| (key.to_owned(), t)));
        for (cfg, target) in manifest.get("target").and_then(toml::Value::as_table).into_iter().flatten() {
            tables.extend(target.get(key).map(|t| (format!("target.'{cfg}'.{key}"), t)));
        }
    }
    for (table, deps) in tables {
        for (dep, spec) in deps.as_table().into_iter().flatten() {
            if !inherited(Some(spec)) {
                found.push(format!(
                    "{name}: [{table}] {dep} isn't `{dep}.workspace = true`: declare it once in [workspace.dependencies]"
                ));
            }
        }
    }
    found
}

/// The workspace, as `cargo metadata --no-deps` describes it.
fn metadata() -> Value {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps", "--offline", "--manifest-path"])
        .arg(repo::root().join("Cargo.toml"))
        .output()
        .expect("cargo metadata runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).expect("cargo metadata prints JSON")
}

fn read_toml(path: &std::path::Path) -> toml::Table {
    std::fs::read_to_string(path)
        .expect("the manifest reads")
        .parse()
        .expect("the manifest is TOML")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(name: &str, deps: &[(&str, Option<&str>)]) -> Value {
        let deps: Vec<Value> = deps.iter().map(|(n, k)| serde_json::json!({ "name": n, "kind": k })).collect();
        serde_json::json!({ "name": name, "dependencies": deps })
    }

    fn check(packages: &[Value]) -> Vec<String> {
        violations(&serde_json::json!({ "packages": packages }))
    }

    #[test]
    fn the_apps_depend_on_the_libraries_and_never_the_reverse() {
        let ok = check(&[
            package("isothrum-compiler", &[("serde", None)]),
            package("isothrum-runner", &[("tokio", None), ("tempfile", Some("dev"))]),
            package("isothrum-wasm", &[("isothrum-compiler", None), ("wasm-bindgen", None)]),
            package("isothrum-cli", &[("isothrum-runner", None), ("clap", None)]),
            package("xtask", &[("isothrum-runner", None)]),
        ]);
        assert!(ok.is_empty(), "{ok:?}");
        let wrong = check(&[
            package("isothrum-runner", &[("isothrum-cli", Some("dev")), ("tauri", None)]),
            package("isothrum-compiler", &[("isothrum-runner", None), ("tokio", None)]),
            package("isothrum-wasm", &[("isothrum-runner", None)]),
            package("isothrum-cli", &[("xtask", None)]),
        ]);
        assert_eq!(wrong.len(), 6, "{wrong:#?}");
    }

    #[test]
    fn a_crate_the_rule_does_not_place_is_named() {
        let found = check(&[package("isothrum-new", &[])]);
        assert_eq!(found, ["isothrum-new: a crate the dependency rule doesn't place yet (xtask/src/workspace.rs)"]);
    }

    #[test]
    fn a_dependency_declared_with_its_own_version_is_named() {
        let manifest: toml::Table = "[package]\nversion.workspace = true\nedition = \"2024\"\n[dependencies]\nserde.workspace = true\n[dev-dependencies]\nproptest = \"1\"\n[target.'cfg(windows)'.dependencies]\nwindows = { version = \"0.62\" }\n"
            .parse()
            .unwrap();
        let found = not_inherited("x", &manifest);
        assert!(found.contains(&"x: package.edition isn't `edition.workspace = true`".to_owned()), "{found:#?}");
        assert!(found.contains(&"x: [lints] isn't `workspace = true`".to_owned()), "{found:#?}");
        assert!(found.iter().any(|f| f.starts_with("x: [dev-dependencies] proptest")), "{found:#?}");
        assert!(
            found.iter().any(|f| f.starts_with("x: [target.'cfg(windows)'.dependencies] windows")),
            "{found:#?}"
        );
        assert!(!found.iter().any(|f| f.contains("serde")), "{found:#?}");
    }

    #[test]
    fn the_repository_keeps_its_dependencies_one_way() {
        let found = violations(&metadata());
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    #[test]
    fn every_crate_inherits_the_workspace() {
        let metadata = metadata();
        let mut found = Vec::new();
        for p in metadata["packages"].as_array().into_iter().flatten() {
            let manifest = read_toml(std::path::Path::new(p["manifest_path"].as_str().unwrap_or("")));
            found.extend(not_inherited(p["name"].as_str().unwrap_or(""), &manifest));
        }
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
