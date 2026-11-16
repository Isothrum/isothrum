//! Reading the crates' sources for the scans: each Rust file, its code without its tests, and
//! its tests by name.

use std::path::{Path, PathBuf};

use crate::repo;

/// One Rust file: where it is (relative to the repository, with `/`) and what it says.
pub(super) struct File {
    pub(super) path: String,
    pub(super) text: String,
}

/// The file at `path` holding `text`: what a guardrail's test reads.
#[cfg(test)]
pub(super) fn file(path: &str, text: &str) -> File {
    File {
        path: path.to_owned(),
        text: text.to_owned(),
    }
}

/// Every `.rs` file under the repository's folder `dir`, sorted.
pub(super) fn files(dir: &str) -> Vec<File> {
    let root = repo::root();
    let mut paths = Vec::new();
    walk(&repo::path_in(&root, dir), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| File {
            path: repo::slashed(p.strip_prefix(&root).unwrap_or(&p)),
            text: std::fs::read_to_string(&p).unwrap_or_default(),
        })
        .collect()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Whether the file is a test's only: a `tests` folder, `tests.rs`, or the helpers the tests
/// share (`testing.rs`, the scripted processes of `fake.rs`).
pub(super) fn is_test_file(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.contains("/tests/") || matches!(name, "tests.rs" | "testing.rs" | "fake.rs")
}

/// `text` without its test modules (`#[cfg(test)]` or `#[cfg(all(test, …))]` before a `mod … {`,
/// up to the `}` that ends it at its own indent) and test-only declarations (the same attribute
/// before one line).
pub(super) fn code(text: &str) -> String {
    let mut out = String::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let test_only = line.trim() == "#[cfg(test)]" || line.trim().starts_with("#[cfg(all(test,");
        if !test_only {
            out.push_str(line);
            out.push('\n');
            continue;
        }
        let Some(next) = lines.next() else { break };
        if next.trim_end().ends_with('{') {
            let indent = next.len() - next.trim_start().len();
            let close = format!("{}}}", " ".repeat(indent));
            for inner in lines.by_ref() {
                if inner.trim_end() == close {
                    break;
                }
            }
        }
    }
    out
}

/// The test named `name` among `files`: its text from its `fn` to the next test (or 120 lines),
/// enough to hold what it asserts.
pub(super) fn test_body<'f>(files: &'f [File], name: &str) -> Option<&'f str> {
    let head = format!("fn {name}(");
    files.iter().find_map(|f| {
        let start = f.text.find(&head)?;
        let rest = f.text.get(start..)?;
        let end = rest
            .get(head.len()..)
            .and_then(|after| after.find("#[test]").into_iter().chain(after.find("#[tokio::test")).min())
            .map_or(rest.len(), |i| i + head.len());
        rest.get(..end)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_module_and_a_test_only_line_are_left_out_of_the_code() {
        let text = "fn a() {}\n#[cfg(test)]\nmod x;\nfn b() {}\n#[cfg(test)]\nmod tests {\n    fn c() {}\n}\nfn d() {}\n#[cfg(all(test, unix))]\nmod real {\n    fn e() {}\n}\n";
        assert_eq!(code(text), "fn a() {}\nfn b() {}\nfn d() {}\n");
        assert!(is_test_file("crates/a/src/x/tests/y.rs") && is_test_file("crates/a/src/x/tests.rs"));
        assert!(!is_test_file("crates/a/src/driver.rs"));
    }
}
