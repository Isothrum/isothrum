//! What clippy can't say about the runner's code, read from its sources (tests left out):
//!
//! - a tool's output is parsed (`serde_json::from_str`/`from_slice`) in an adapter only: the
//!   drivers, `tool_output`, and the readers [`ADAPTERS`] names; a file that parses JSON that is
//!   no tool's output says what it parses in [`NOT_TOOL_OUTPUT`];
//! - the script a `sh -c` (or `ssh … -c`, `vagrant ssh -c`) runs comes from the shell scripts'
//!   module (`driver::shells`) or is one [`SCRIPTS`] names, never a `format!` assembled where the
//!   command is;
//! - a script that runs as root (the shell scripts' module, the probes [`SCRIPTS`] names) chains
//!   no command with `&&`: under `set -e` and in a probe a failed left side passes silently, so
//!   a step says each command's failure with `;` and an `if`.

use super::source;

/// The crates whose code the scans read.
const CRATES: [&str; 3] = ["crates/isothrum-runner/src", "crates/isothrum-machine/src", "crates/isothrum-cli/src"];

/// Where a tool's answer is read, beside the drivers and `tool_output` (the tool-output module's
/// own list of the documents read beside their tool).
const ADAPTERS: [(&str, &str); 4] = [
    (
        "crates/isothrum-runner/src/driver/",
        "each driver reads its own tool's documents (Compose's ps and config, Terraform's state)",
    ),
    ("crates/isothrum-runner/src/tool_output/", "what the tools print, one reader per tool"),
    (
        "crates/isothrum-runner/src/reap/tags.rs",
        "the clouds' CLIs' listings, read in the reaper's one reader",
    ),
    (
        "crates/isothrum-runner/src/graphical/proxmox/answer.rs",
        "Proxmox's API answers, read in one place",
    ),
];

/// Files that parse JSON that is no tool's output.
const NOT_TOOL_OUTPUT: [(&str, &str); 3] = [
    ("crates/isothrum-runner/src/store.rs", "the home's own files, which the runner wrote"),
    ("crates/isothrum-runner/src/store/db.rs", "the store's own rows, which the runner wrote"),
    (
        "crates/isothrum-runner/src/process/shim.rs",
        "the Docker CLI's config file, the user's settings, not an answer",
    ),
];

/// The scripts a `-c` may run besides the shell scripts' module's, by file and the expression
/// passed: a probe (also read for `&&`), the manifest's own guest script, and an option that is
/// no script.
const SCRIPTS: [(&str, &str, &str); 3] = [
    ("crates/isothrum-runner/src/driver/vagrant/actions.rs", "&halting_probe(", "fn halting_probe"),
    (
        "crates/isothrum-runner/src/driver/vagrant.rs",
        "script",
        "the manifest's guest step, as Isoloom wrote it",
    ),
    (
        "crates/isothrum-runner/src/graphical/vagrant.rs",
        "LIBVIRT_URI",
        "virsh's `-c` is its connection, not a script",
    ),
];

/// The shell scripts' module: every literal in it is a script.
const SHELLS: &str = "crates/isothrum-runner/src/driver/shells.rs";

/// Each crate's files that aren't a test's, as code without their test modules.
fn code_files() -> Vec<source::File> {
    CRATES
        .iter()
        .flat_map(|dir| source::files(dir))
        .filter(|f| !source::is_test_file(&f.path))
        .map(|f| source::File {
            text: source::code(&f.text),
            path: f.path,
        })
        .collect()
}

/// The line `at` (a byte offset) is on, from 1.
fn line_of(text: &str, at: usize) -> usize {
    text.get(..at).map_or(0, |t| t.matches('\n').count()) + 1
}

/// Every JSON parse outside an adapter and the files that parse no tool's output.
fn parses_outside_adapters(files: &[source::File]) -> Vec<String> {
    let allowed = |path: &str| ADAPTERS.iter().any(|(p, _)| path.starts_with(p)) || NOT_TOOL_OUTPUT.iter().any(|(p, _)| path == *p);
    let mut found = Vec::new();
    for f in files.iter().filter(|f| !allowed(&f.path)) {
        for call in ["serde_json::from_str", "serde_json::from_slice"] {
            found.extend(f.text.match_indices(call).map(|(at, _)| {
                format!(
                    "{}:{}: {call} outside an adapter: a tool's answer is read in tool_output (or the driver's own reader); JSON that is no tool's goes in NOT_TOOL_OUTPUT (xtask/src/guardrails/scans.rs)",
                    f.path,
                    line_of(&f.text, at)
                )
            }));
        }
    }
    found
}

/// The expression after each `"-c",` in `text`, with its offset.
fn dash_c_operands(text: &str) -> Vec<(usize, &str)> {
    text.match_indices("\"-c\"")
        .filter_map(|(at, m)| {
            let rest = text.get(at + m.len()..)?.trim_start().strip_prefix(',')?.trim_start();
            let end = rest.find([']', ',', ')']).unwrap_or(rest.len());
            Some((at, rest.get(..end)?.trim()))
        })
        .collect()
}

/// Every `-c` whose script comes from neither the shell scripts' module nor [`SCRIPTS`].
fn scripts_built_in_place(files: &[source::File]) -> Vec<String> {
    let mut found = Vec::new();
    for f in files {
        for (at, operand) in dash_c_operands(&f.text) {
            let listed = SCRIPTS.iter().any(|(p, o, _)| f.path == *p && operand == *o);
            if operand.contains("shells::") || f.path == SHELLS || (listed && !operand.contains("format!")) {
                continue;
            }
            found.push(format!(
                "{}:{}: `-c {operand}`: a shell script comes from driver::shells (quoted there), never assembled where the command is",
                f.path,
                line_of(&f.text, at)
            ));
        }
    }
    found
}

/// The string literals of `text` (between unescaped double quotes; raw strings read as plain).
fn literals(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = None;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match (start, c) {
            (Some(_), '\\') if !escaped => escaped = true,
            (Some(s), '"') if !escaped => {
                out.extend(text.get(s..i));
                start = None;
            }
            (None, '"') => start = Some(i + 1),
            _ => escaped = false,
        }
    }
    out
}

/// The text of `fn name` in `text`: from its `fn` to the `}` that ends it at the start of a line.
fn function<'t>(text: &'t str, head: &str) -> Option<&'t str> {
    let start = text.find(head)?;
    let rest = text.get(start..)?;
    rest.get(..rest.find("\n}").map_or(rest.len(), |i| i + 2))
}

/// Every `&&` in a script that runs as root: the shell scripts' module and the probes.
fn chains_in_root_scripts(files: &[source::File]) -> Vec<String> {
    let mut scripts: Vec<(&str, &str)> = files.iter().filter(|f| f.path == SHELLS).map(|f| (f.path.as_str(), f.text.as_str())).collect();
    for (path, _, head) in SCRIPTS.iter().filter(|(_, _, h)| h.starts_with("fn ")) {
        let body = files.iter().find(|f| f.path == *path).and_then(|f| function(&f.text, head));
        scripts.push((*path, body.unwrap_or("")));
    }
    let mut found = Vec::new();
    for (path, text) in scripts {
        found.extend(
            literals(text)
                .into_iter()
                .filter(|l| l.contains("&&"))
                .map(|l| format!("{path}: `{l}`: a root script chains no command with `&&` (`;` and an `if` say each failure)")),
        );
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
    fn a_parse_outside_an_adapter_is_named_with_its_line() {
        let files = [
            file("crates/isothrum-runner/src/driver/compose/ps.rs", "serde_json::from_str(x)"),
            file("crates/isothrum-runner/src/store.rs", "serde_json::from_slice(&b)"),
            file(
                "crates/isothrum-runner/src/graphical/native.rs",
                "let a = 1;\nlet v = serde_json::from_str(&out.stdout)?;",
            ),
        ];
        let found = parses_outside_adapters(&files);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("crates/isothrum-runner/src/graphical/native.rs:2: serde_json::from_str"),
            "{found:?}"
        );
    }

    #[test]
    fn a_script_assembled_where_the_command_is_is_named() {
        let files = [
            file(
                "crates/isothrum-runner/src/driver/compose.rs",
                r#"["exec", s, "sh", "-c", super::shells::CONTAINER_SHELL]"#,
            ),
            file("crates/isothrum-runner/src/driver/vagrant.rs", r#"["ssh", d, "-c", script]"#),
            file(
                "crates/isothrum-runner/src/relay.rs",
                r#"ProcessSpec::new("sh", ["-c", &format!("nc {host} {port}")])"#,
            ),
            file(
                "crates/isothrum-runner/src/driver/vagrant.rs",
                r#"["ssh", d, "-c", &format!("cd {x}; {script}")]"#,
            ),
        ];
        let found = scripts_built_in_place(&files);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("-c &format!(\"nc {host} {port}\""), "{found:?}");
    }

    #[test]
    fn a_chain_in_a_root_script_is_named() {
        let files = [
            file(SHELLS, "const A: &str = \"if a; then b; fi\";\nconst B: &str = \"mkdir -p /x && chown u /x\";"),
            file(
                "crates/isothrum-runner/src/driver/vagrant/actions.rs",
                "fn halting_probe() -> String {\n    format!(\"test -e {H} && echo halting\")\n}\nfn other() { \"a && b\" }",
            ),
        ];
        let found = chains_in_root_scripts(&files);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(
            found[0].contains("mkdir -p /x && chown u /x") && found[1].contains("test -e {H} && echo halting"),
            "{found:?}"
        );
        assert_eq!(literals(r#"a "x\"y" b "z""#), [r#"x\"y"#, "z"]);
    }

    #[test]
    fn the_runner_parses_tool_output_in_its_adapters_only() {
        let found = parses_outside_adapters(&code_files());
        assert!(found.is_empty(), "{}", found.join("\n"));
    }

    #[test]
    fn the_runners_shell_scripts_come_from_its_shell_module() {
        let files = code_files();
        let mut found = scripts_built_in_place(&files);
        found.extend(chains_in_root_scripts(&files));
        assert!(found.is_empty(), "{}", found.join("\n"));
        for (path, operand, _) in SCRIPTS {
            assert!(
                files
                    .iter()
                    .any(|f| f.path == path && dash_c_operands(&f.text).iter().any(|(_, o)| *o == operand)),
                "{path}: no `-c {operand}` any more: take it out of SCRIPTS"
            );
        }
    }
}
