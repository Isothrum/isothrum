//! The product boundary: Isothrum runs the files a generator wrote, it doesn't link the
//! generator. No package of this workspace's graph depends on an Isoloom crate (`isoloom-*`),
//! whether a crate of the workspace names it or a dependency pulls it in. The graph is read from
//! `Cargo.lock`, which holds every package of the workspace's resolution: no tool runs, and no
//! crate needs to be downloaded first.

use serde_json::Value;

/// The prefix of every Isoloom crate.
const ISOLOOM: &str = "isoloom-";

/// One dependency edge of the resolved graph: who depends on whom, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    from: String,
    to: String,
    kind: Option<String>,
}

/// The resolved graph `Cargo.lock`'s text `lock` holds, in `cargo metadata`'s shape (`packages`,
/// `workspace_members`, `resolve.nodes`): a package with no `source` is one of the workspace's
/// own, and a dependency (`name`, `name version` or `name version (source)`) is the package of
/// that name and version.
fn lock_graph(lock: &str) -> Result<Value, String> {
    let doc: toml::Table = lock.parse().map_err(|e| format!("Cargo.lock isn't TOML: {e}"))?;
    let packages = doc.get("package").and_then(toml::Value::as_array).ok_or("Cargo.lock lists no package")?;
    let text = |p: &toml::Value, key: &str| p.get(key).and_then(toml::Value::as_str).map(str::to_owned);
    let listed: Vec<(String, String)> = packages
        .iter()
        .map(|p| (text(p, "name").unwrap_or_default(), text(p, "version").unwrap_or_default()))
        .collect();
    let id = |name: &str, version: &str| format!("{name} {version}");
    let resolved = |dep: &str| {
        let mut words = dep.split_whitespace();
        let name = words.next().unwrap_or_default();
        let version = words.next();
        listed
            .iter()
            .find(|(n, v)| n == name && version.is_none_or(|want| want == v))
            .map_or_else(|| name.to_owned(), |(n, v)| id(n, v))
    };
    let mut nodes = Vec::new();
    let mut members = Vec::new();
    for (p, (name, version)) in packages.iter().zip(&listed) {
        if p.get("source").is_none() {
            members.push(Value::String(id(name, version)));
        }
        let deps: Vec<Value> = p
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(toml::Value::as_str)
            .map(|d| serde_json::json!({ "pkg": resolved(d), "dep_kinds": [{ "kind": null }] }))
            .collect();
        nodes.push(serde_json::json!({ "id": id(name, version), "deps": deps }));
    }
    Ok(serde_json::json!({
        "packages": listed.iter().map(|(n, v)| serde_json::json!({ "id": id(n, v), "name": n })).collect::<Vec<_>>(),
        "workspace_members": members,
        "resolve": { "nodes": nodes },
    }))
}

/// Every edge of `cargo metadata`'s resolved graph, by package name.
fn edges(metadata: &Value) -> Vec<Edge> {
    let name_of = |id: &str| {
        metadata["packages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|p| p["id"].as_str() == Some(id))
            .and_then(|p| p["name"].as_str())
            .unwrap_or(id)
            .to_owned()
    };
    let mut found = Vec::new();
    for node in metadata["resolve"]["nodes"].as_array().into_iter().flatten() {
        let from = name_of(node["id"].as_str().unwrap_or(""));
        for dep in node["deps"].as_array().into_iter().flatten() {
            let to = name_of(dep["pkg"].as_str().unwrap_or(""));
            for kind in dep["dep_kinds"].as_array().into_iter().flatten() {
                found.push(Edge {
                    from: from.clone(),
                    to: to.clone(),
                    kind: kind["kind"].as_str().map(str::to_owned),
                });
            }
        }
    }
    found
}

/// The workspace's own crates, by name.
fn members(metadata: &Value) -> Vec<String> {
    let ids: Vec<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["id"].as_str().is_some_and(|id| ids.contains(&id)))
        .filter_map(|p| p["name"].as_str().map(str::to_owned))
        .collect()
}

/// Every edge into an Isoloom crate, as sentences.
fn violations(metadata: &Value) -> Vec<String> {
    let members = members(metadata);
    let all = edges(metadata);
    all.iter()
        .filter(|e| e.to.starts_with(ISOLOOM) && !e.from.starts_with(ISOLOOM))
        .map(|e| {
            let kind = e.kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default();
            if members.contains(&e.from) {
                format!("{} depends on {}{kind}: Isothrum runs native files and links no Isoloom crate", e.from, e.to)
            } else {
                format!(
                    "{} (a dependency of the workspace) pulls in {}{kind}: Isothrum links no Isoloom crate",
                    e.from, e.to
                )
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo;

    /// A dependency of a test's package: its name and kind.
    type Dep = (&'static str, Option<&'static str>);

    /// A test's package: its name and its dependencies.
    type Package = (&'static str, Vec<Dep>);

    /// Metadata of `packages` (name, its dependencies with their kind), all of them members but
    /// the Isoloom crates and `outsiders`.
    fn graph(packages: &[Package], outsiders: &[&str]) -> Value {
        let id = |n: &str| format!("{n} 1.0.0");
        let mut names: Vec<&str> = packages.iter().map(|(n, _)| *n).collect();
        names.extend(packages.iter().flat_map(|(_, deps)| deps.iter().map(|(d, _)| *d)));
        names.sort_unstable();
        names.dedup();
        let nodes: Vec<Value> = packages
            .iter()
            .map(|(n, deps)| {
                let deps: Vec<Value> = deps
                    .iter()
                    .map(|(d, k)| serde_json::json!({ "pkg": id(d), "dep_kinds": [{ "kind": k }] }))
                    .collect();
                serde_json::json!({ "id": id(n), "deps": deps })
            })
            .collect();
        let members: Vec<String> = names
            .iter()
            .filter(|n| !n.starts_with(ISOLOOM) && !outsiders.contains(*n))
            .map(|n| id(n))
            .collect();
        serde_json::json!({
            "packages": names.iter().map(|n| serde_json::json!({ "id": id(n), "name": n })).collect::<Vec<_>>(),
            "workspace_members": members,
            "resolve": { "nodes": nodes },
        })
    }

    fn check(extra: &[Package], outsiders: &[&str]) -> Vec<String> {
        violations(&graph(extra, outsiders))
    }

    #[test]
    fn a_workspace_with_no_isoloom_edge_passes_and_any_edge_fails() {
        assert_eq!(check(&[("isothrum-runner", vec![("tokio", None)])], &[]), Vec::<String>::new());
        let found = check(&[("isothrum-cli", vec![("isoloom-render", None)])], &[]);
        assert_eq!(
            found,
            ["isothrum-cli depends on isoloom-render: Isothrum runs native files and links no Isoloom crate"]
        );
        let dev = check(&[("isothrum-runner", vec![("isoloom-manifest", Some("dev"))])], &[]);
        assert_eq!(dev.len(), 1, "a test's edge is an edge too: {dev:?}");
    }

    #[test]
    fn an_isoloom_crate_a_dependency_pulls_in_fails() {
        let found = check(
            &[("isothrum-runner", vec![("helper", None)]), ("helper", vec![("isoloom-spec", None)])],
            &["helper"],
        );
        assert_eq!(
            found,
            ["helper (a dependency of the workspace) pulls in isoloom-spec: Isothrum links no Isoloom crate"]
        );
    }

    #[test]
    fn a_lock_files_packages_and_their_dependencies_are_the_graph() {
        let lock = r#"
version = 4

[[package]]
name = "isothrum-runner"
version = "3.0.0"
dependencies = ["helper 1.0.0", "serde"]

[[package]]
name = "helper"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = ["isoloom-spec 2.0.0 (registry+https://github.com/rust-lang/crates.io-index)"]

[[package]]
name = "helper"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "isoloom-spec"
version = "2.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;
        let graph = lock_graph(lock).expect("a lock file");
        assert_eq!(members(&graph), ["isothrum-runner"], "the package with no source");
        let runner_deps: Vec<&str> = graph["resolve"]["nodes"][0]["deps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|d| d["pkg"].as_str())
            .collect();
        assert_eq!(
            runner_deps,
            ["helper 1.0.0", "serde 1.0.0"],
            "each dependency is the package of its name and version"
        );
        assert_eq!(
            violations(&graph),
            ["helper (a dependency of the workspace) pulls in isoloom-spec: Isothrum links no Isoloom crate"],
            "the edge through helper 1.0.0, by name and version"
        );
        assert!(lock_graph("not toml [").is_err_and(|e| e.starts_with("Cargo.lock isn't TOML")));
    }

    #[test]
    fn no_isothrum_crate_depends_on_an_isoloom_crate() {
        let lock = std::fs::read_to_string(repo::path_in(&repo::root(), "Cargo.lock")).expect("the workspace's Cargo.lock");
        let metadata = lock_graph(&lock).expect("Cargo.lock reads");
        assert!(!members(&metadata).is_empty(), "the workspace's own crates are read");
        let found = violations(&metadata);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
