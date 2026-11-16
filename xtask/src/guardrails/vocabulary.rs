//! What the crates say: Isothrum runs any Compose, Vagrant or Terraform project, whoever wrote it,
//! so no file under `crates/` (code, tests, fixtures, READMEs) names Isoloom, nor uses the words of
//! an application built on Isothrum (lab, CTF, player, attack box). A test's data uses neutral
//! names: a project, a stack, a toolbox. The documentation (`docs/`, the root's Markdown) names no
//! generator either, but for one line of the README; the changelog and the migration guides are
//! records, and keep their words.

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

/// An application's words: Isothrum knows deployments, machines, services and places, never what
/// an application built on it calls them. Each is matched as a word of an identifier too
/// (`LabRun`, `attack_box`).
const WORDS: [&str; 8] = ["lab", "labs", "ctf", "ctfs", "cyberctf", "player", "players", "attackbox"];

/// The lines allowed to use one of [`WORDS`]: a file and the word, and why it isn't the
/// application's word there.
const ALLOWED: &[(&str, &str, &str)] = &[(
    "crates/isothrum-machine/src/providers/unavailable.rs",
    "ctf",
    "a test that the reason never says an application's words",
)];

/// The words of `line`: its runs of letters and digits, each split where a capital follows a
/// small letter (`LabRun` is `lab`, `run`), in lower case.
fn words(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut word = String::new();
    let mut last: Option<char> = None;
    for c in line.chars() {
        let boundary = !c.is_ascii_alphanumeric() || (c.is_ascii_uppercase() && last.is_some_and(|l| l.is_ascii_lowercase()));
        if boundary && !word.is_empty() {
            found.push(std::mem::take(&mut word));
        }
        if c.is_ascii_alphanumeric() {
            word.push(c.to_ascii_lowercase());
        }
        last = Some(c);
    }
    if !word.is_empty() {
        found.push(word);
    }
    found
}

/// The application words of `line`: those of [`WORDS`], and `attack` followed by `box`.
fn application_words(line: &str) -> Vec<String> {
    let words = words(line);
    let mut found: Vec<String> = words.iter().filter(|w| WORDS.contains(&w.as_str())).cloned().collect();
    found.extend(
        words
            .windows(2)
            .filter(|w| w[0] == "attack" && (w[1] == "box" || w[1] == "boxes"))
            .map(|_| "attack box".to_owned()),
    );
    found
}

/// Each line of `files` that uses an application's word, unless [`ALLOWED`] lets that file use it.
fn application_vocabulary(files: &[File]) -> Vec<String> {
    let mut found = Vec::new();
    for f in files {
        for (n, line) in f.text.lines().enumerate() {
            for word in application_words(line) {
                if !ALLOWED.iter().any(|(path, allowed, _)| *path == f.path && *allowed == word) {
                    found.push(format!("{}:{}: `{word}` is an application's word: say what Isothrum calls it", f.path, n + 1));
                }
            }
        }
    }
    found
}

/// The documentation: every file under `docs/` and the Markdown files at the repository's root.
/// It may describe what Isothrum runs, never name a generator: the changelog and the migration
/// guides are records of releases that did, and keep their words.
fn documentation() -> Vec<File> {
    let mut files = source::root_markdown();
    files.extend(source::texts("docs"));
    files.retain(|f| f.path != "CHANGELOG.md" && !f.path.starts_with("docs/migrating-to-"));
    files
}

/// The one line of the documentation allowed to name a generator: the README's, saying Isothrum
/// runs its output like anything else.
const DOCS_ALLOWED: (&str, &str) = ("README.md", "Isothrum can also run the projects Isoloom generates");

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

    #[test]
    fn an_application_word_is_named_in_a_name_an_identifier_or_a_phrase_and_a_longer_word_is_not() {
        let files = [
            text("crates/a/src/lib.rs", "struct LabRun;\nlet label = available();\n"),
            text(
                "crates/a/tests/b.rs",
                "// the player's attack box\nlet x = \"cyberctf-web\";\nfn attack_box() {}\n",
            ),
            text(
                "crates/isothrum-machine/src/providers/unavailable.rs",
                "assert!(!said.contains(\"Cyber CTF\"));\n",
            ),
        ];
        let said = |at: &str, word: &str| format!("{at}: `{word}` is an application's word: say what Isothrum calls it");
        assert_eq!(
            application_vocabulary(&files),
            [
                said("crates/a/src/lib.rs:1", "lab"),
                said("crates/a/tests/b.rs:1", "player"),
                said("crates/a/tests/b.rs:1", "attack box"),
                said("crates/a/tests/b.rs:2", "cyberctf"),
                said("crates/a/tests/b.rs:3", "attack box"),
            ]
        );
        assert_eq!(words("TestLab web-1.lab"), ["test", "lab", "web", "1", "lab"]);
    }

    #[test]
    fn no_file_under_crates_uses_an_applications_words() {
        let found = application_vocabulary(&source::texts(DIR));
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    #[test]
    fn no_document_names_isoloom_but_the_readmes_one_line() {
        let files = documentation();
        assert!(
            files.iter().any(|f| f.path == "README.md") && files.iter().any(|f| f.path.starts_with("docs/")),
            "the walk is broken"
        );
        assert!(!files.iter().any(|f| f.path == "CHANGELOG.md"), "the changelog is a record");
        let (path, line) = DOCS_ALLOWED;
        let readme = files.iter().find(|f| f.path == path).map_or("", |f| f.text.as_str());
        assert!(readme.contains(line), "the allowed line is gone from {path}: take it off DOCS_ALLOWED");
        let rest: Vec<File> = files
            .into_iter()
            .map(|f| File {
                text: f.text.lines().filter(|l| !(f.path == path && l.contains(line))).collect::<Vec<_>>().join("\n"),
                path: f.path,
            })
            .collect();
        let found = isoloom_mentions(&rest);
        assert!(found.is_empty(), "{}", found.join("\n"));
    }
}
