//! The commands that need no tool, on a runner whose tools are scripted to be never called. The
//! operations' own end-to-end tests (a Compose project up, parked, down) are the runner's
//! (`run/tests.rs`); the binary's, against a stand-in `docker`, are `tests/commands.rs`.

use std::sync::Arc;

use clap::Parser;
use isothrum_runner::fake::FakeProcesses;
use isothrum_runner::{FakePorts, Runner, RunnerConfig};

use crate::args::Cli;
use crate::commands;
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::term::{Term, Trail};
use crate::testing::Buf;

struct Setup {
    home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    term: Term,
    out: Buf,
}

/// A Compose project in a folder of its own, and a runner on an empty script: a tool these
/// commands run fails the test.
fn setup() -> Setup {
    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(run.path().join("compose.yaml"), "services:\n  web:\n    image: nginx\n").unwrap();
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::new(FakeProcesses::script().build());
    config.ports = Arc::new(FakePorts::new([]));
    let out = Buf::default();
    let term = Term::new(Box::new(out.clone()), Box::new(Buf::default()));
    Setup {
        home,
        run,
        runner: Runner::new(config).unwrap(),
        term,
        out,
    }
}

impl Setup {
    /// `command_line`'s verdict, on this runner.
    async fn verdict(&self, command_line: &[&str]) -> Result<Verdict> {
        let cli = Cli::parse_from(command_line);
        commands::run(&cli, || Ok(&self.runner), &self.term, &Trail::default(), &|_| None).await
    }
}

#[tokio::test]
async fn completions_build_no_runner_so_touch_no_home() {
    let out = Buf::default();
    let term = Term::new(Box::new(out.clone()), Box::new(Buf::default()));
    let cli = Cli::parse_from(["isothrum", "completions", "bash"]);
    let no_runner = || -> Result<Runner> { panic!("completions built a runner") };
    let verdict = commands::run(&cli, no_runner, &term, &Trail::default(), &|_| None).await;
    assert_eq!(verdict.unwrap(), Verdict::Pass);
    assert!(out.take().contains("complete -F"), "a bash completion script");
}

/// Regression: a malformed `--name` exited 1 with the code `run_name`, where the exit-code
/// reference promises 2 for a malformed value.
#[tokio::test]
async fn a_malformed_name_is_a_usage_error() {
    let s = setup();
    let run = s.run.path().display().to_string();
    let err = s.verdict(&["isothrum", "status", "--name", "Not A Name", &run]).await.unwrap_err();
    assert!(matches!(&err, Error::Usage(why) if why.starts_with("--name: ")), "{err}");
    assert_eq!(crate::exit::code(&Err(err)), std::process::ExitCode::from(crate::exit::USAGE));
}

/// Regression: `key` ignored `--json`, printing the bare key a script reading JSON can't parse.
#[tokio::test]
async fn key_prints_the_public_key_or_with_json_one_document() {
    const KEY: &str = "ssh-ed25519 AAAAfake isothrum";
    let s = setup();
    let ssh = s.home.path().join("ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), "private").unwrap();
    std::fs::write(ssh.join("id_ed25519.pub"), format!("{KEY}\n")).unwrap();

    assert_eq!(s.verdict(&["isothrum", "key"]).await.unwrap(), Verdict::Pass);
    assert_eq!(s.out.take(), format!("{KEY}\n"));

    assert_eq!(s.verdict(&["isothrum", "key", "--json"]).await.unwrap(), Verdict::Pass);
    let said = s.out.take();
    assert_eq!(said.lines().count(), 1, "one document: {said}");
    let document: serde_json::Value = serde_json::from_str(&said).unwrap();
    assert_eq!(document, serde_json::json!({ "public_key": KEY }));
}

/// Regression (#127): a Proxmox console's websocket carried its VNC ticket, the password
/// `--show-password` is for, into the terminal whatever the flag.
#[test]
fn a_proxmox_websocket_hides_its_ticket_without_show_password() {
    let ticket = "PVEVNC:6553F0A4::dGlja2V0";
    let url = "wss://pve:8006/api2/json/nodes/pve/qemu/101/vncwebsocket?port=5900&vncticket=PVEVNC%3A6553F0A4%3A%3AdGlja2V0";
    let hidden = commands::ticket_shown(url, ticket, false);
    assert!(!hidden.contains("dGlja2V0"), "{hidden}");
    assert!(hidden.ends_with("vncticket=<hidden: --show-password prints it>"), "{hidden}");
    assert_eq!(commands::ticket_shown(url, ticket, true), url);
}
