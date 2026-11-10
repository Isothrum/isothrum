//! The reference pages written from the code, so they can't drift from it: the command line's
//! (`docs/reference/cli.md`, from its clap grammar) and its exit codes with every error code
//! (`docs/reference/exit-codes.md`, from [`crate::exit`] and the runner's `ErrorCode`). A test
//! fails when a committed page is stale; `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli
//! reference` writes them.

use std::fmt::Write as _;
use std::path::PathBuf;

use clap::CommandFactory;
use isothrum_runner::ErrorCode;

use crate::args::Cli;

/// The line every generated page starts with.
const GENERATED: &str = "<!-- Generated from the code: don't edit. `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference` writes it. -->";

/// A reference page's path in the checkout.
fn page(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/reference").join(name)
}

/// Checks the committed page `name` is `text`, or writes it when asked to.
fn check_or_write(name: &str, text: &str) {
    let path = page(name);
    if std::env::var_os("ISOTHRUM_WRITE_REFERENCE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == text,
        "docs/reference/{name} is stale: run `ISOTHRUM_WRITE_REFERENCE=1 cargo test -p isothrum-cli reference`"
    );
}

/// The command line's reference: every command, its usage and its arguments.
fn cli_page() -> String {
    let mut root = Cli::command().bin_name("isothrum");
    root.build();
    let mut out = format!("{GENERATED}\n\n# The `isothrum` command line\n\n{}\n\n", sentence(&help_of(&root)));
    write!(out, "```text\n{}\n```\n\n", root.render_usage().to_string().trim_start_matches("Usage: ")).unwrap();
    out.push_str("Every command takes the global options. Exit codes: [exit-codes.md](exit-codes.md); environment: [environment.md](environment.md).\n\n## Global options\n\n");
    out.push_str(&arguments(root.get_arguments().filter(|a| a.is_global_set())));
    for sub in root.get_subcommands_mut().filter(|s| !s.is_hide_set() && s.get_name() != "help") {
        let name = sub.get_name().to_owned();
        write!(out, "\n## `isothrum {name}`\n\n{}\n\n", sentence(&help_of(sub))).unwrap();
        write!(out, "```text\n{}\n```\n\n", sub.render_usage().to_string().trim_start_matches("Usage: ")).unwrap();
        out.push_str(&arguments(sub.get_arguments().filter(|a| !a.is_global_set() && !a.is_hide_set())));
    }
    out
}

/// A command's long help, else its short one.
fn help_of(cmd: &clap::Command) -> String {
    cmd.get_long_about().or(cmd.get_about()).map(ToString::to_string).unwrap_or_default()
}

/// One bullet per argument: its form, its help, its values and default.
fn arguments<'a>(args: impl Iterator<Item = &'a clap::Arg>) -> String {
    let mut out = String::new();
    for arg in args.filter(|a| !["help", "version"].contains(&a.get_id().as_str())) {
        let help = arg.get_long_help().or(arg.get_help()).map(ToString::to_string).unwrap_or_default();
        write!(out, "- `{}`: {}", form(arg), sentence(&help.replace('\n', " "))).unwrap();
        let values: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| format!("`{}`", v.get_name()))
            .collect();
        if !values.is_empty() {
            write!(out, " One of {}.", values.join(", ")).unwrap();
        }
        let flag = matches!(arg.get_action(), clap::ArgAction::SetTrue);
        let defaults: Vec<String> = arg.get_default_values().iter().map(|v| v.to_string_lossy().into_owned()).collect();
        if !defaults.is_empty() && !flag {
            write!(out, " Default: `{}`.", defaults.join(",")).unwrap();
        }
        out.push('\n');
    }
    out
}

/// `help` ending with a full stop (clap drops a doc comment's last one).
fn sentence(help: &str) -> String {
    let help = help.trim_end();
    if help.ends_with(['.', ')', ':']) {
        help.to_owned()
    } else {
        format!("{help}.")
    }
}

/// How an argument is written: `--name <VALUE>`, `--flag` or `<PATH>`.
fn form(arg: &clap::Arg) -> String {
    let value = arg
        .get_value_names()
        .and_then(|v| v.first())
        .map_or_else(|| arg.get_id().as_str().to_uppercase(), ToString::to_string);
    let takes_value = arg.get_num_args().is_none_or(|n| n.takes_values());
    match arg.get_long() {
        Some(long) if takes_value && !matches!(arg.get_action(), clap::ArgAction::SetTrue) => format!("--{long} <{value}>"),
        Some(long) => format!("--{long}"),
        None => format!("<{value}>"),
    }
}

/// The exit codes, then every error code with its cause and remedy.
fn exit_codes_page() -> String {
    let mut out = format!("{GENERATED}\n\n# Exit codes and error codes\n\n## Exit codes\n\n| Code | When |\n| ---- | ---- |\n");
    for (code, when) in EXIT_CODES {
        writeln!(out, "| {code} | {when} |").unwrap();
    }
    out.push_str(
        "\n## Error codes\n\nAn operation that fails ends its event stream with an `error` event whose `code` is one \
         of these (with `--json`, also a failure before any operation began). The code is stable; the message is \
         for a person.\n\n| Code | Cause | Remedy |\n| ---- | ----- | ------ |\n",
    );
    for (code, cause) in error_codes() {
        writeln!(
            out,
            "| `{}` | {} | {} |",
            code.as_str(),
            cause.replace('\n', " "),
            remedy(code).unwrap_or_default()
        )
        .unwrap();
    }
    out
}

/// Every error code, with its documentation, as the runner's schema lists them.
fn error_codes() -> Vec<(ErrorCode, String)> {
    let schema = serde_json::to_value(schemars::schema_for!(ErrorCode)).unwrap();
    schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                serde_json::from_value(v["const"].clone()).unwrap(),
                v["description"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect()
}

/// Every exit code, with when the command gives it: what the exit-code reference says
/// (`exit`'s tests give each from `exit::code`).
const EXIT_CODES: [(u8, &str); 5] = [
    (0, "The command reached its goal: the run is up (or parked, down, …), every check passed."),
    (
        1,
        "It didn't: an operation failed (its `error` event says why, with its code), a check \
         failed, or the runner refused (a run that is busy, a manifest it can't read).",
    ),
    (
        2,
        "A usage error: an unknown option, a value missing or malformed, a run folder with no \
         manifest (the `error` event's code is `usage`).",
    ),
    (
        130,
        "SIGINT (Ctrl-C) cancelled it: the tools it started were asked to stop (SIGINT) and ended \
         once they had, 30 s later or on a second signal (at once on Windows), and the operation \
         ended with an `error` event, code `cancelled` (on Windows: Ctrl-C).",
    ),
    (
        143,
        "SIGTERM cancelled it (a CI runner cancelling its job), as for 130 (on Windows: \
         Ctrl-Break, the console closed, a log-off or a shutdown).",
    ),
];

/// What to do about `code`.
fn remedy(code: ErrorCode) -> Option<&'static str> {
    Some(match code {
        ErrorCode::ManifestInvalid => {
            "Generate the run's files again with the Isoloom this runner was built for; a value the message names is the run's own to fix."
        }
        ErrorCode::ManifestVersion => "Use the Isothrum release that reads this manifest version, or regenerate the run with a matching Isoloom.",
        ErrorCode::Unsupported => "Upgrade Isothrum, generate the run for another target, or ask the run for what its target does.",
        ErrorCode::Io => {
            "Check the path in the message: free space, permissions, the runner's home (`--home`, `ISOTHRUM_HOME`); for a port, the open-file limit (`ulimit -n`) and the loopback."
        }
        ErrorCode::Locked => "Wait for the other operation on the run (or the other start's slot pick) to end, or stop the process running it.",
        ErrorCode::ToolMissing => "Install the tool, or put it on `PATH`.",
        ErrorCode::ToolFailed => "Read the step's last lines (its `log` events, `isothrum events`): fix what the tool says, then run the command again.",
        ErrorCode::ToolTooOld => "Upgrade the tool to the version the message names.",
        ErrorCode::Timeout => {
            "See the event's `timeout`: a step stuck with no output, or one that needs longer. Check the network (proxy) and the engine, then retry."
        }
        ErrorCode::AlreadyRunning => "Use it as it is, `down` it first, or `up --replace`.",
        ErrorCode::RunningElsewhere => "Act on the run with the `--host-label` it was started with.",
        ErrorCode::PortInUse => "Stop what holds the port, or start the run with other published ports.",
        ErrorCode::NotParkable => "Park it in the other mode (`--mode`), or `down` it.",
        ErrorCode::NothingToResume => "Start it again with `up`.",
        ErrorCode::NotRunning => "Start it with `up` (or `resume` a parked run) first.",
        ErrorCode::NotReady => "Read the `ready` events: the service the condition waits for didn't come up. `status`, then `down` and `up` again.",
        ErrorCode::SecretMissing => "Give the value: `--secret-from-env NAME[=VAR]`, with the variable set.",
        ErrorCode::ParamInvalid => "Fix the `--param NAME=VALUE` the message names (a value the manifest allows).",
        ErrorCode::RunName => "Give `--name` 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit.",
        ErrorCode::ProviderRefused => "Pick a provider the run supports and this host has (`--provider`), with its Vagrant plugin installed.",
        ErrorCode::ElevationFailed => {
            "Approve the Windows permission prompt, or run as a member of Hyper-V Administrators; the message says what ended an elevated session."
        }
        ErrorCode::MachineUnknown => "Name a machine of the manifest.",
        ErrorCode::MachineAmbiguous => "Name the machine (`--machine`): the message lists the run's.",
        ErrorCode::NetworkUnknown => "Name a network of the manifest.",
        ErrorCode::SlotsFull => "`down` a run on the same place first.",
        ErrorCode::AdoptRefused => "The message says why the deployment can't be taken over; start the run with `up` instead.",
        ErrorCode::Unreachable => "Check the machine booted and its network (SSH, `--ssh-jump`), then retry.",
        ErrorCode::NotSealed => "`down` the run, which isn't ready, then `up` again.",
        ErrorCode::Cancelled => "Nothing was left running by the tools; `status` shows what the run holds, and `up` clears what a cut start left.",
        ErrorCode::Usage => "Fix the command line: `isothrum <command> --help`.",
        _ => return None,
    })
}

#[test]
fn the_committed_cli_reference_is_current() {
    check_or_write("cli.md", &cli_page());
}

#[test]
fn the_exit_codes_reference_lists_every_code_the_command_line_gives() {
    use crate::cancel::Signal;
    use crate::error::Error;
    use crate::exit::{Verdict, code};
    let given = [
        code(&Ok(Verdict::Pass)),
        code(&Ok(Verdict::Fail)),
        code(&Err(Error::Usage("no manifest".into()))),
        code(&Err(Error::Cancelled(Signal::Interrupt))),
        code(&Err(Error::Cancelled(Signal::Terminate))),
    ];
    let listed: Vec<std::process::ExitCode> = EXIT_CODES.iter().map(|(c, _)| std::process::ExitCode::from(*c)).collect();
    assert_eq!(given.to_vec(), listed);
}

#[test]
fn the_committed_exit_codes_reference_is_current() {
    check_or_write("exit-codes.md", &exit_codes_page());
}

#[test]
fn every_error_code_has_a_cause_and_a_remedy() {
    let codes = error_codes();
    assert!(codes.len() > 20, "{codes:?}");
    for (code, cause) in codes {
        assert!(!cause.is_empty() && remedy(code).is_some(), "{code}");
    }
}
