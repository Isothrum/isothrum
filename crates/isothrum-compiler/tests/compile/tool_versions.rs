//! The tool minimums the docs state (the README's requirements, the supported tool versions page)
//! are the ones Isoloom's run manifests ask for, read from what Isoloom generates for the fixture
//! drawings, not written down a second time: a minimum Isoloom raises fails here until the docs
//! follow.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::testing::{Outcome, accepted, yaml};

/// The drawings whose targets, together, run every tool with a minimum: Compose, Vagrant,
/// Terraform on a server, ESXi and the clouds.
const DRAWINGS: [&str; 2] = ["vlans", "windows-domain"];

/// The name the docs give each tool a manifest names.
const DOC_NAMES: [(&str, &str); 3] = [("docker-compose", "Docker Compose"), ("vagrant", "Vagrant"), ("terraform", "Terraform")];

/// The repository's file at `path`.
fn repo_file(path: &str) -> Outcome<String> {
    let root: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    Ok(std::fs::read_to_string(root.join(path))?)
}

/// Each (tool as the docs name it, minimum) the run manifests of the drawings' targets hold.
fn minimums() -> Outcome<BTreeSet<(String, String)>> {
    let mut out = BTreeSet::new();
    for name in DRAWINGS {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(format!("{name}.json"));
        let compiled = accepted(&serde_json::from_str(&std::fs::read_to_string(&path)?)?)?;
        let project = isoloom_targets::Lab::parse(&yaml(&compiled)).map_err(|p| format!("{name}: {p:?}"))?;
        for file in isoloom_targets::generate_all(&project).0.iter().filter(|f| f.path.ends_with("/run.json")) {
            let manifest: serde_json::Value = serde_json::from_str(&file.contents)?;
            let tools = manifest.get("tools").and_then(serde_json::Value::as_array).ok_or("no tools")?;
            for tool in tools {
                let text = |key: &str| tool.get(key).and_then(serde_json::Value::as_str);
                let (Some(program), Some(min)) = (text("name"), text("min_version")) else {
                    continue;
                };
                let doc = DOC_NAMES
                    .iter()
                    .find(|(n, _)| *n == program)
                    .ok_or_else(|| format!("{}: no doc name for {program}", file.path))?
                    .1;
                out.insert((doc.to_owned(), min.to_owned()));
            }
        }
    }
    Ok(out)
}

/// The `x.y.z` versions in `text`.
fn versions(text: &str) -> BTreeSet<String> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '.'))
        .map(|w| w.trim_matches('.'))
        .filter(|w| w.split('.').count() == 3 && w.split('.').all(|p| !p.is_empty()))
        .map(str::to_owned)
        .collect()
}

#[test]
fn the_tool_versions_page_states_the_minimums_isoloom_writes() -> Outcome {
    let wanted = minimums()?;
    assert_eq!(
        wanted.iter().map(|(tool, _)| tool.as_str()).collect::<BTreeSet<_>>().len(),
        DOC_NAMES.len(),
        "{wanted:?}"
    );
    let page = repo_file("docs/reference/tool-versions.md")?;
    // The table's rows: `| Tool | Minimum | Used by | Probe |`.
    let rows: BTreeSet<(String, String)> = page
        .lines()
        .filter_map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            let (tool, min) = (cells.get(1)?, cells.get(2)?);
            versions(min).into_iter().next().map(|v| ((*tool).to_owned(), v))
        })
        .collect();
    assert_eq!(rows, wanted, "the page's minimums against the manifests'");
    Ok(())
}

#[test]
fn the_readmes_requirements_state_the_minimums_isoloom_writes() -> Outcome {
    let wanted: BTreeSet<String> = minimums()?.into_iter().map(|(_, min)| min).collect();
    let readme = repo_file("README.md")?;
    let section = readme.split("## Requirements").nth(1).ok_or("no Requirements section")?;
    let section = section.split("\n## ").next().unwrap_or(section);
    assert_eq!(versions(section), wanted, "the README's minimums against the manifests'");
    Ok(())
}
