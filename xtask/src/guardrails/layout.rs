//! Where the tests live, as CONTRIBUTING's "Tests" section says: a module's unit tests in its
//! `mod tests`, inline or in `<module>/tests.rs` (themes in `<module>/tests/<theme>.rs`), never a
//! file found by `#[path]` or named `*_tests.rs`; a test is named by the sentence it proves, never
//! `test_…`.

use super::source;

/// The folders whose Rust files the rule reads: every crate's and the xtask's.
const DIRS: [&str; 2] = ["crates", "xtask/src"];

/// Each place in `files` that breaks the layout, with its file (and line).
fn misplaced(files: &[source::File]) -> Vec<String> {
    let mut found = Vec::new();
    for f in files {
        if f.path.ends_with("_tests.rs") {
            found.push(format!(
                "{}: a test file is `<module>/tests.rs` or `<module>/tests/<theme>.rs`, not `*_tests.rs`",
                f.path
            ));
        }
        for (n, line) in f.text.lines().enumerate() {
            let line = line.trim_start();
            if line.starts_with("#[path") {
                found.push(format!(
                    "{}:{}: `#[path]`: a module's tests are found where Rust looks, `<module>/tests.rs`",
                    f.path,
                    n + 1
                ));
            }
            if line.starts_with("fn test_") || line.starts_with("async fn test_") {
                found.push(format!("{}:{}: a test is named by what it proves, without `test_`", f.path, n + 1));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> source::File {
        source::File {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_path_attribute_a_tests_suffix_and_a_test_prefix_are_each_named() {
        let files = [
            file("crates/a/src/net.rs", "fn a() {}\n#[cfg(test)]\n#[path = \"net_tests.rs\"]\nmod tests;\n"),
            file("crates/a/src/net_tests.rs", "#[test]\nfn test_it_binds() {}\n"),
            file("crates/a/src/net/tests.rs", "#[test]\nfn a_port_in_use_is_refused() {}\n"),
        ];
        assert_eq!(
            misplaced(&files),
            [
                "crates/a/src/net.rs:3: `#[path]`: a module's tests are found where Rust looks, `<module>/tests.rs`",
                "crates/a/src/net_tests.rs: a test file is `<module>/tests.rs` or `<module>/tests/<theme>.rs`, not `*_tests.rs`",
                "crates/a/src/net_tests.rs:2: a test is named by what it proves, without `test_`",
            ]
        );
    }

    #[test]
    fn the_workspaces_tests_live_where_contributing_says() {
        let files: Vec<source::File> = DIRS.iter().flat_map(|d| source::files(d)).collect();
        assert!(files.len() > 100, "the rule read {} files: the walk is broken", files.len());
        let found = misplaced(&files);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
