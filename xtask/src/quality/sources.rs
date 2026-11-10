//! What the sources themselves say: the workspace's crates (from `cargo metadata`), the files
//! over the size limit, and the exceptions (`#[expect]`, under a `cfg_attr` too).

use std::path::{Path, PathBuf};

use super::{Counts, Crate, add};
use crate::repo::path_in;
use crate::{Res, repo};

/// Lines of code a source file may have.
const MAX_CODE_LINES: usize = 400;

/// The fuzz crate: a workspace of its own (`fuzz/`), outside `cargo metadata`'s, held to the same
/// rules.
pub(crate) const FUZZ: (&str, &str) = ("isothrum-fuzz", "fuzz");

/// The folders of a crate whose files the size rule counts: its library and binaries, and the
/// fuzz crate's targets.
const SOURCE_DIRS: [&str; 2] = ["src", "fuzz_targets"];

/// The workspace's crates, from `cargo metadata`, and the fuzz crate.
pub(crate) fn crates(root: &Path) -> Res<Vec<Crate>> {
    let metadata = repo::metadata(root)?;
    let base = repo::slashed(Path::new(metadata.get("workspace_root").and_then(serde_json::Value::as_str).unwrap_or("")));
    let packages = metadata.get("packages").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    Ok(packages
        .iter()
        .filter_map(|p| {
            let name = p.get("name")?.as_str()?.to_owned();
            let manifest = repo::slashed(Path::new(p.get("manifest_path")?.as_str()?));
            let dir = manifest.strip_suffix("/Cargo.toml")?.strip_prefix(&base)?.trim_start_matches('/').to_owned();
            Some(Crate { name, dir })
        })
        .chain([Crate {
            name: FUZZ.0.to_owned(),
            dir: FUZZ.1.to_owned(),
        }])
        .collect())
}

/// Counts the files over the size limit (in a crate's [`SOURCE_DIRS`], its unit tests' files
/// included) and the `#[expect]` exceptions (anywhere in the crate).
pub(crate) fn measure(root: &Path, crates: &[Crate], counts: &mut Counts) -> Res<()> {
    for c in crates {
        let dir = path_in(root, &c.dir);
        for sources in SOURCE_DIRS.iter().map(|s| dir.join(s)).filter(|d| d.is_dir()) {
            for file in rust_files(&sources)? {
                if code_lines(&std::fs::read_to_string(&file)?) > MAX_CODE_LINES {
                    add(counts, "file/over-400-loc", &c.name);
                }
            }
        }
        for file in rust_files(&dir)? {
            let text = std::fs::read_to_string(&file)?;
            for _ in 0..expectations(&text) {
                add(counts, "exception/expect", &c.name);
            }
        }
    }
    Ok(())
}

/// Every `.rs` file under `dir`, recursively, build folders aside.
///
/// # Errors
///
/// A folder or an entry of it can't be read: a file left out would count as none.
fn rust_files(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", dir.display())))? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                out.extend(rust_files(&path)?);
            }
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
    Ok(out)
}

/// The lines of `src` that count: not blank, not a comment, not inside the inline test module.
/// rustfmt closes a top-level module with a `}` alone at column 0, which ends the test block.
pub(crate) fn code_lines(src: &str) -> usize {
    let mut count = 0;
    let mut in_tests = false;
    let mut previous = "";
    for line in src.lines() {
        let trimmed = line.trim();
        if in_tests {
            in_tests = line != "}";
        } else if previous == "#[cfg(test)]" && trimmed.starts_with("mod tests") && trimmed.ends_with('{') {
            in_tests = true;
            count -= 1;
        } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
            count += 1;
        }
        previous = trimmed;
    }
    count
}

/// How many lint exceptions `src` holds: each `expect` attribute, on its own (`#[expect(…)]`,
/// `#![expect(…)]`) or under a condition (`#[cfg_attr(…, expect(…))]`), wherever it stands (an
/// item, a statement, a parameter). What comments and literals say isn't one.
fn expectations(src: &str) -> usize {
    let code = code_only(src);
    let mut count = 0;
    let mut rest = code.as_str();
    while let Some(at) = rest.find('#') {
        rest = rest.get(at + 1..).unwrap_or_default();
        let attribute = rest.strip_prefix('!').unwrap_or(rest).trim_start();
        if let Some(inner) = attribute.strip_prefix('[') {
            count += expects_in(until_closed(inner));
        }
    }
    count
}

/// How many `expect`s the attribute `meta` is: one when it is `expect(…)`, those of each of its
/// attributes when it is `cfg_attr(condition, …)`, else none.
fn expects_in(meta: &str) -> usize {
    let meta = meta.trim_start();
    let name_len = meta.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')).unwrap_or(meta.len());
    let (name, rest) = meta.split_at(name_len);
    let Some(args) = rest.trim_start().strip_prefix('(').map(until_closed) else {
        return 0;
    };
    match name {
        "expect" => 1,
        "cfg_attr" => top_level(args).into_iter().skip(1).map(expects_in).sum(),
        _ => 0,
    }
}

/// `text` up to the bracket that closes the one just before it: `(`, `[` and `{` nest.
fn until_closed(text: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => return text.get(..at).unwrap_or(text),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    text
}

/// `args` split at its commas outside brackets.
fn top_level(args: &str) -> Vec<&str> {
    let (mut parts, mut depth, mut start) = (Vec::new(), 0usize, 0);
    for (at, c) in args.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(args.get(start..at).unwrap_or_default());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(args.get(start..).unwrap_or_default());
    parts
}

/// `src` with its comments and its string and character literals blanked out (each character
/// a space, its line breaks kept): only code is left to read attributes from.
fn code_only(src: &str) -> String {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let skip = match (c, chars.get(i + 1).copied()) {
            ('/', Some('/')) => chars.iter().skip(i).take_while(|ch| **ch != '\n').count(),
            ('/', Some('*')) => block_comment(&chars, i),
            ('"', _) => quoted(&chars, i, '"'),
            ('r' | 'b', _) if !continues_word(&chars, i) => raw_or_byte(&chars, i),
            ('\'', _) => char_literal(&chars, i),
            _ => 0,
        };
        if skip == 0 {
            out.push(c);
            i += 1;
        } else {
            out.extend(chars.iter().skip(i).take(skip).map(|ch| if *ch == '\n' { '\n' } else { ' ' }));
            i += skip;
        }
    }
    out
}

/// Whether the character at `i` continues an identifier (so an `r` or a `b` there is no prefix).
fn continues_word(chars: &[char], i: usize) -> bool {
    i.checked_sub(1).and_then(|p| chars.get(p)).is_some_and(|c| c.is_alphanumeric() || *c == '_')
}

/// The length of the block comment at `i` (they nest).
fn block_comment(chars: &[char], i: usize) -> usize {
    let (mut depth, mut j) = (0usize, i);
    while j < chars.len() {
        match (chars.get(j), chars.get(j + 1)) {
            (Some('/'), Some('*')) => {
                depth += 1;
                j += 2;
            }
            (Some('*'), Some('/')) => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return j - i;
                }
            }
            _ => j += 1,
        }
    }
    j - i
}

/// The length of the literal at `i` that `quote` closes, `\` escaping the next character.
fn quoted(chars: &[char], i: usize, quote: char) -> usize {
    let mut j = i + 1;
    while let Some(&c) = chars.get(j) {
        j += if c == '\\' { 2 } else { 1 };
        if c == quote {
            return j - i;
        }
    }
    j - i
}

/// The length of the raw string (`r"…"`, `r#"…"#`, `br"…"`), byte string (`b"…"`) or byte
/// (`b'…'`) at `i`; none when there is none.
fn raw_or_byte(chars: &[char], i: usize) -> usize {
    let after_b = if chars.get(i) == Some(&'b') { i + 1 } else { i };
    match chars.get(after_b) {
        Some('"') => return quoted(chars, after_b, '"') + after_b - i,
        Some('\'') if after_b > i => return quoted(chars, after_b, '\'') + after_b - i,
        Some('r') if after_b > i || chars.get(i) == Some(&'r') => {}
        _ => return 0,
    }
    let start = if chars.get(after_b) == Some(&'r') { after_b + 1 } else { after_b };
    let hashes = chars.iter().skip(start).take_while(|c| **c == '#').count();
    if chars.get(start + hashes) != Some(&'"') {
        return 0;
    }
    let mut j = start + hashes + 1;
    while j < chars.len() {
        if chars.get(j) == Some(&'"') && (1..=hashes).all(|k| chars.get(j + k) == Some(&'#')) {
            return j + hashes + 1 - i;
        }
        j += 1;
    }
    j - i
}

/// The length of the character literal at `i`; none for a lifetime (`'a`).
fn char_literal(chars: &[char], i: usize) -> usize {
    match (chars.get(i + 1), chars.get(i + 2)) {
        (Some('\\'), _) => quoted(chars, i, '\''),
        (Some(_), Some('\'')) => 3,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a folder that couldn't be read counted as one with no file, so the ratchet
    /// could under-count and pass.
    #[test]
    fn a_folder_that_cant_be_read_fails_the_count() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("lib.rs"), "").unwrap();
        assert_eq!(rust_files(dir.path()).unwrap(), [dir.path().join("src").join("lib.rs")]);
        assert!(rust_files(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn comments_blanks_and_the_test_module_are_not_counted() {
        let src = "//! Doc.\n\nfn a() {}\n    // note\n/// item doc\nfn b() {\n}\n#[cfg(test)]\nmod tests {\n    fn t() {\n    }\n}\nfn after() {}\n";
        assert_eq!(code_lines(src), 4);
    }

    #[test]
    fn an_expectation_counts_once() {
        assert_eq!(expectations("#[expect(clippy::x, reason = \"y\")]\nfn a() {}\n    #![expect(dead_code)]\n"), 2);
    }

    /// Regression: only an attribute starting its line with `expect` was counted, so one under a
    /// `cfg_attr`, or on a parameter, slipped past the ratchet.
    #[test]
    fn an_expectation_under_a_condition_or_on_a_parameter_counts() {
        let conditioned = "fn f(\n    #[cfg_attr(not(windows), expect(unused_variables, reason = \"x\"))] p: u8,\n) {}\n";
        assert_eq!(expectations(conditioned), 1);
        assert_eq!(expectations("fn f(#[expect(unused)] p: u8, q: u8) {}\n"), 1);
        let nested = "#[cfg_attr(test, cfg_attr(unix, expect(dead_code)), expect(unused))]\nfn f() {}\n";
        assert_eq!(expectations(nested), 2);
        assert_eq!(expectations("#[cfg_attr(test, derive(Debug))]\n#[cfg(expect)]\nstruct S;\n"), 0);
    }

    #[test]
    fn what_comments_and_literals_say_is_no_expectation() {
        let said = concat!(
            "// #[expect(a)]\n/* #[expect(b)] /* #[expect(c)] */ */\n/// `#[expect(d)]`\n",
            "const A: &str = \"#[expect(e)] \\\" #[expect(f)]\";\n",
            "const B: &str = r#\"#[expect(g)] \" #[expect(h)]\"#;\n",
            "const C: &[u8] = b\"#[expect(i)]\";\n",
            "const D: char = '#'; fn f<'a>(x: &'a str) {}\n",
            "#[expect(counted)]\nfn g() {}\n",
        );
        assert_eq!(expectations(said), 1);
    }
}
