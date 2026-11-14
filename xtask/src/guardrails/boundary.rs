//! The product boundary: Isothrum runs the files a generator wrote, it doesn't link the
//! generator. No package of this workspace's graph depends on an Isoloom crate (`isoloom-*`),
//! whether a crate of the workspace names it or a dependency pulls it in.

use serde_json::Value;

use crate::repo;

/// The prefix of every Isoloom crate.
const ISOLOOM: &str = "isoloom-";

/// One dependency edge of the resolved graph: who depends on whom, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edge {
    from: String,
    to: String,
    kind: Option<String>,
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

    fn check(extra: Vec<Package>, outsiders: &[&str]) -> Vec<String> {
        violations(&graph(&extra, outsiders))
    }

    #[test]
    fn a_workspace_with_no_isoloom_edge_passes_and_any_edge_fails() {
        assert!(check(vec![("isothrum-runner", vec![("tokio", None)])], &[]).is_empty());
        let found = check(vec![("isothrum-cli", vec![("isoloom-render", None)])], &[]);
        assert_eq!(
            found,
            ["isothrum-cli depends on isoloom-render: Isothrum runs native files and links no Isoloom crate"]
        );
        let dev = check(vec![("isothrum-runner", vec![("isoloom-manifest", Some("dev"))])], &[]);
        assert_eq!(dev.len(), 1, "a test's edge is an edge too: {dev:?}");
    }

    #[test]
    fn an_isoloom_crate_a_dependency_pulls_in_fails() {
        let found = check(
            vec![("isothrum-runner", vec![("helper", None)]), ("helper", vec![("isoloom-spec", None)])],
            &["helper"],
        );
        assert_eq!(
            found,
            ["helper (a dependency of the workspace) pulls in isoloom-spec: Isothrum links no Isoloom crate"]
        );
    }

    #[test]
    fn no_isothrum_crate_depends_on_an_isoloom_crate() {
        let metadata = repo::metadata_resolved(&repo::root()).expect("cargo metadata runs and prints JSON");
        let found = violations(&metadata);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
