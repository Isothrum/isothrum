//! What the crates say: Isothrum runs any Compose, Vagrant or Terraform project, whoever wrote it,
//! so no file under `crates/` (code, tests, fixtures, READMEs) names Isoloom. A test's data uses
//! neutral names, not a generator's; the documentation under `docs/` may name it, as one
//! generator among others.

use std::path::Path;

use super::source::{self, File};

/// The folder every file of which the rule reads.
const DIR: &str = "crates";

/// Each line of `files` that names Isoloom (in any case, inside a word too: `ISOLOOM_KEY`,
/// `.isoloom/`), with its file and line.
fn isoloom_mentions(files: &[File]) -> Vec<String> {
    files
        .iter()
        .flat_map(|f| {
            f.text
                .lines()
                .enumerate()
                .filter(|(_, line)| line.to_ascii_lowercase().contains("isoloom"))
                .map(|(n, _)| format!("{}:{}: names Isoloom: Isothrum assumes no generator's names", f.path, n + 1))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardrails::source::file as text;

    #[test]
    fn a_line_naming_isoloom_in_any_case_or_inside_a_word_is_named() {
        let files = [
            text("crates/a/src/lib.rs", "fn a() {}\n// as Isoloom writes it\n"),
            text("crates/a/tests/b.rs", "let key = \"ISOLOOM_KEY\";\nlet dir = \"env/.isoloom/docker\";\n"),
            text("crates/a/README.md", "Runs a Compose project, whoever wrote it.\n"),
        ];
        assert_eq!(
            isoloom_mentions(&files),
            [
                "crates/a/src/lib.rs:2: names Isoloom: Isothrum assumes no generator's names",
                "crates/a/tests/b.rs:1: names Isoloom: Isothrum assumes no generator's names",
                "crates/a/tests/b.rs:2: names Isoloom: Isothrum assumes no generator's names",
            ]
        );
    }

    #[test]
    fn no_file_under_crates_names_isoloom() {
        let files = source::texts(DIR);
        assert!(files.len() > 100, "the rule read {} files: the walk is broken", files.len());
        assert!(
            files.iter().any(|f| Path::new(&f.path).extension().is_some_and(|e| e == "json")),
            "fixtures are read too"
        );
        let found = isoloom_mentions(&files);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
