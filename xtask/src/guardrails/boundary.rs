//! The product boundary: Isothrum runs the files a generator wrote, it doesn't link the
//! generator. No package of this workspace's graph depends on an Isoloom crate (`isoloom-*`),
//! whether a crate of the workspace names it or a dependency pulls it in, except the edges listed
//! in [`UNTIL_THE_MANIFEST_GOES`].

use serde_json::Value;

use crate::repo;

/// The prefix of every Isoloom crate.
const ISOLOOM: &str = "isoloom-";

/// The Isoloom edges this workspace still has, each one (crate, Isoloom crate, kind: `None` for a
/// normal dependency, `Some("dev")` for a test's): the run manifest's types and the compiler's
/// and editor's reading of the spec and the targets. The no-manifest removal (Isothrum reading
/// each tool's native files, not Isoloom's run manifest) takes them out and empties this list; a
/// new edge fails now, and an edge gone must leave the list (so it only shrinks).
const UNTIL_THE_MANIFEST_GOES: [(&str, &str, Option<&str>); 11] = [
    ("isothrum-compiler", "isoloom-manifest", None),
    ("isothrum-compiler", "isoloom-spec", None),
    ("isothrum-compiler", "isoloom-targets", Some("dev")),
    ("isothrum-runner", "isoloom-manifest", None),
    ("isothrum-runner", "isoloom-targets", Some("dev")),
    ("isothrum-machine", "isoloom-manifest", None),
    ("isothrum-cli", "isoloom-manifest", None),
    ("isothrum-wasm", "isoloom-manifest", None),
    ("isothrum-wasm", "isoloom-plan", None),
    ("isothrum-wasm", "isoloom-spec", None),
    ("isothrum-wasm", "isoloom-targets", None),
];

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

/// Every edge into an Isoloom crate that the list doesn't hold, and every listed edge that is
/// gone, as sentences.
fn violations(metadata: &Value) -> Vec<String> {
    let members = members(metadata);
    let all = edges(metadata);
    let listed = |e: &Edge| {
        UNTIL_THE_MANIFEST_GOES
            .iter()
            .any(|(f, t, k)| e.from == *f && e.to == *t && e.kind.as_deref() == *k)
    };
    let mut found: Vec<String> = all
        .iter()
        .filter(|e| e.to.starts_with(ISOLOOM) && !e.from.starts_with(ISOLOOM))
        .filter(|e| !listed(e))
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
        .collect();
    found.extend(
        UNTIL_THE_MANIFEST_GOES
            .iter()
            .filter(|(f, t, k)| !all.iter().any(|e| e.from == *f && e.to == *t && e.kind.as_deref() == *k))
            .map(|(f, t, _)| format!("{f} no longer depends on {t}: take the edge out of UNTIL_THE_MANIFEST_GOES (xtask/src/guardrails/boundary.rs)")),
    );
    found
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

    /// Today's edges, all listed: no violation.
    fn today() -> Vec<Package> {
        let mut by_crate: Vec<Package> = Vec::new();
        for (from, to, kind) in &UNTIL_THE_MANIFEST_GOES {
            match by_crate.iter_mut().find(|(f, _)| *f == *from) {
                Some((_, deps)) => deps.push((*to, *kind)),
                None => by_crate.push((*from, vec![(*to, *kind)])),
            }
        }
        by_crate
    }

    fn check(extra: Vec<Package>, outsiders: &[&str]) -> Vec<String> {
        let mut packages = today();
        packages.extend(extra);
        violations(&graph(&packages, outsiders))
    }

    #[test]
    fn todays_listed_edges_pass_and_a_new_one_fails() {
        assert!(check(vec![], &[]).is_empty(), "{:#?}", check(vec![], &[]));
        let new = check(vec![("isothrum-cli", vec![("isoloom-render", None)])], &[]);
        assert_eq!(
            new,
            ["isothrum-cli depends on isoloom-render: Isothrum runs native files and links no Isoloom crate"]
        );
        let other_kind = check(vec![("isothrum-runner", vec![("isoloom-manifest", Some("dev"))])], &[]);
        assert_eq!(other_kind.len(), 1, "a listed edge of another kind is a new edge: {other_kind:?}");
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
    fn a_listed_edge_that_is_gone_must_leave_the_list() {
        let mut base = today();
        base.retain(|(n, _)| *n != "isothrum-cli");
        let found = violations(&graph(&base, &[]));
        assert_eq!(
            found,
            ["isothrum-cli no longer depends on isoloom-manifest: take the edge out of UNTIL_THE_MANIFEST_GOES (xtask/src/guardrails/boundary.rs)"]
        );
    }

    #[test]
    fn no_isothrum_crate_depends_on_an_isoloom_crate_beyond_the_listed_edges() {
        let metadata = repo::metadata_resolved(&repo::root()).expect("cargo metadata runs and prints JSON");
        let found = violations(&metadata);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
