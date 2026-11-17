//! What clippy can't say about the runner's code, read from its sources (tests left out):
//!
//! - a tool's output is parsed (`serde_json::from_str`/`from_slice`) in an adapter only: the
//!   tools' adapters (`cli/`), `tool_output`, and the readers [`ADAPTERS`] names; a file that
//!   parses JSON that is no tool's output says what it parses in [`NOT_TOOL_OUTPUT`];
//! - the script a shell's `-c` (`sh -c`, `bash -c`, `vagrant ssh <machine> -c`) runs comes from
//!   the shell scripts' module (`console::shells`) or is one [`SCRIPTS`] names, never a `format!`
//!   assembled where the command is; another program's `-c` (virsh's connection) is no script;
//! - a script that runs as root (the shell scripts' module's) chains no command with `&&`: under
//!   `set -e` a failed left side passes silently, so a script says each command's failure with
//!   `;` and an `if`.

use super::source;

/// The crates whose code the scans read.
const CRATES: [&str; 5] = [
    "crates/isothrum-runner/src",
    "crates/isothrum-machine/src",
    "crates/isothrum-cli/src",
    "crates/isothrum-service/src",
    "crates/isothrum-client/src",
];

/// Where a tool's answer is read, beside `tool_output` (the tool-output module's own list of the
/// documents read beside their tool).
const ADAPTERS: [(&str, &str); 4] = [
    (
        "crates/isothrum-runner/src/cli/",
        "one adapter per tool: its argv, its spawn and the parse of its answers into typed values",
    ),
    ("crates/isothrum-runner/src/tool_output/", "what the tools print, one reader per tool"),
    (
        "crates/isothrum-machine/src/cloud",
        "one adapter per cloud CLI: its argv, its runs and the parse of its answers about an account",
    ),
    (
        "crates/isothrum-runner/src/formats/terraform/tunnelled.rs",
        "a module's `consoles` output, the value `terraform output` gave, read in its one reader",
    ),
];

/// Files that parse JSON that is no tool's output.
const NOT_TOOL_OUTPUT: [(&str, &str); 11] = [
    (
        "crates/isothrum-runner/src/formats/terraform/required.rs",
        "a Terraform module's own `*.tfvars.json` files, read for the names they set",
    ),
    (
        "crates/isothrum-runner/src/project/variables.rs",
        "a Terraform module's own `*.tf.json` files, which its author wrote",
    ),
    (
        "crates/isothrum-runner/src/proxmox.rs",
        "Proxmox VE's API answers, read by the runner's own HTTPS client: no tool's output",
    ),
    (
        "crates/isothrum-machine/src/place/proxmox/api.rs",
        "Proxmox VE's API answers, read by isothrum-machine's own HTTPS client: no tool's output",
    ),
    ("crates/isothrum-runner/src/store.rs", "the home's own files, which the runner wrote"),
    ("crates/isothrum-runner/src/store/db.rs", "the store's own rows, which the runner wrote"),
    (
        "crates/isothrum-runner/src/store/db/mirror.rs",
        "the store's mirror file, which the runner wrote",
    ),
    (
        "crates/isothrum-runner/src/process/shim.rs",
        "the Docker CLI's config file, the user's settings, not an answer",
    ),
    (
        "crates/isothrum-client/src/discovery.rs",
        "the service's discovery file, which the service wrote",
    ),
    ("crates/isothrum-client/src/wire.rs", "the service protocol's frames, Isothrum's own"),
    (
        "crates/isothrum-machine/src/providers/virtiofsd.rs",
        "QEMU's vhost-user descriptors, the files libvirt reads to start a virtiofsd",
    ),
];

/// The scripts a shell's or an ssh's `-c` may run besides the shell scripts' module's, by file
/// and the expression passed.
const SCRIPTS: [(&str, &str, &str); 6] = [
    (
        "crates/isothrum-machine/src/install/macos.rs",
        "script",
        "a macOS install's own script: a cask fetched and opened, or Homebrew's update then QEMU, Homebrew's path single-quoted",
    ),
    (
        "crates/isothrum-machine/src/install/vmware_utility.rs",
        "&mac_script(&file",
        "the VMware utility's macOS install: a pinned file name and digest, its root half an argument",
    ),
    (
        "crates/isothrum-machine/src/install/linux.rs",
        "text",
        "a root step's script: a `RootScript` of the install's own scripts (`install::linux::scripts`), its prelude first",
    ),
    (
        "crates/isothrum-machine/src/install/linux.rs",
        "\"rm -rf \\\"$HOME/google-cloud-sdk\\\"\"",
        "the per-user Google Cloud SDK's removal, a constant",
    ),
    (
        "crates/isothrum-runner/src/cli/vagrant.rs",
        "script",
        "the adapter's parameter: a login shell of the shell scripts' module, or the caller's command with each word quoted (`console::sh_word`)",
    ),
    (
        "crates/isothrum-runner/src/formats/compose.rs",
        "shell",
        "a container shell of the shell scripts' module, picked on the line before",
    ),
];

/// The programs whose `-c` takes a script: the shells, and `ssh` (`vagrant ssh <machine> -c`).
/// Another program's `-c` (virsh's connection) is no script.
const SCRIPT_TAKERS: [&str; 3] = ["\"sh\"", "\"bash\"", "\"ssh\""];

/// The shell scripts' module: every literal in it is a script.
const SHELLS: &str = "crates/isothrum-runner/src/console/shells.rs";

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

/// The expression after each `"-c"` of a shell or an ssh in `text` (`"-c",`, or the same as an
/// owned string, `"-c".to_owned(),` or `"-c".into(),`), with its offset. The `-c` is a script's
/// when its line, up to it, names a [`SCRIPT_TAKERS`] (the argument list it is in).
fn dash_c_operands(text: &str) -> Vec<(usize, &str)> {
    text.match_indices("\"-c\"")
        .filter_map(|(at, m)| {
            let rest = text.get(at + m.len()..)?;
            let rest = [".to_owned()", ".to_string()", ".into()"]
                .iter()
                .fold(rest, |r, owned| r.strip_prefix(owned).unwrap_or(r));
            let rest = rest.trim_start().strip_prefix(',')?.trim_start();
            let end = rest.find([']', ',', ')']).unwrap_or(rest.len());
            let before = text.get(..at)?;
            let line = before.get(before.rfind('\n').map_or(0, |i| i + 1)..)?;
            SCRIPT_TAKERS.iter().any(|p| line.contains(p)).then_some(())?;
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

/// Every `&&` in a script that runs as root: the shell scripts' module's.
fn chains_in_root_scripts(files: &[source::File]) -> Vec<String> {
    files
        .iter()
        .filter(|f| f.path == SHELLS)
        .flat_map(|f| {
            literals(&f.text)
                .into_iter()
                .filter(|l| l.contains("&&"))
                .map(|l| {
                    format!(
                        "{}: `{l}`: a root script chains no command with `&&` (`;` and an `if` say each failure)",
                        f.path
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::guardrails::source::file;

    #[test]
    fn a_parse_outside_an_adapter_is_named_with_its_line() {
        let files = [
            file("crates/isothrum-runner/src/cli/compose/ps.rs", "serde_json::from_str(x)"),
            file("crates/isothrum-runner/src/store.rs", "serde_json::from_slice(&b)"),
            file(
                "crates/isothrum-runner/src/graphical/native.rs",
                "let a = 1;\nlet v = serde_json::from_str(&out.stdout)?;",
            ),
        ];
        let found = parses_outside_adapters(&files);
        assert_eq!(
            found,
            [
                "crates/isothrum-runner/src/graphical/native.rs:2: serde_json::from_str outside an adapter: a tool's answer is read in tool_output (or the driver's own reader); JSON that is no tool's goes in NOT_TOOL_OUTPUT (xtask/src/guardrails/scans.rs)"
            ]
        );
    }

    #[test]
    fn a_script_assembled_where_the_command_is_is_named() {
        let files = [
            file(
                "crates/isothrum-runner/src/formats/compose.rs",
                r#"["exec", s, "sh", "-c", super::shells::CONTAINER_SHELL]"#,
            ),
            file("crates/isothrum-runner/src/cli/vagrant.rs", r#"argv(["ssh", d, "-c", script])"#),
            file(
                "crates/isothrum-runner/src/net.rs",
                r#"ProcessSpec::new("sh", ["-c", &format!("nc {host} {port}")])"#,
            ),
            file(
                "crates/isothrum-runner/src/cli/vagrant.rs",
                r#"argv(["ssh", d, "-c", &format!("cd {x}; {script}")])"#,
            ),
            file("crates/isothrum-runner/src/formats/x.rs", r#"vec!["bash".to_owned(), "-c".to_owned(), built]"#),
        ];
        let found = scripts_built_in_place(&files);
        let operands: Vec<&str> = found.iter().map(|f| f.split('`').nth(1).unwrap()).collect();
        assert_eq!(
            operands,
            [r#"-c &format!("nc {host} {port}""#, r#"-c &format!("cd {x}; {script}""#, "-c built"],
            "{found:?}"
        );
    }

    #[test]
    fn another_programs_dash_c_is_no_script() {
        let virsh = r#"args.extend(["-c".to_owned(), uri.to_owned()]);"#;
        assert_eq!(dash_c_operands(virsh), []);
        let ssh = r#"argv(["ssh", machine, "-c", script])"#;
        assert_eq!(dash_c_operands(ssh), [(22, "script")]);
    }

    #[test]
    fn a_chain_in_a_root_script_is_named() {
        let files = [
            file(SHELLS, "const A: &str = \"if a; then b; fi\";\nconst B: &str = \"mkdir -p /x && chown u /x\";"),
            file("crates/isothrum-runner/src/formats/vagrant.rs", "fn other() { \"a && b\" }"),
        ];
        let found = chains_in_root_scripts(&files);
        assert_eq!(
            found,
            [format!(
                "{SHELLS}: `mkdir -p /x && chown u /x`: a root script chains no command with `&&` (`;` and an `if` say each failure)"
            )]
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
