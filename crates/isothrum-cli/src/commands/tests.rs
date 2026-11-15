//! The commands that need no tool, on a runner whose tools are scripted. The operations' own
//! end-to-end tests (a Compose project up, parked, down) are the runner's (`run/tests.rs`).

use std::sync::Arc;

use clap::Parser;
use isothrum_runner::fake::{FakeProcesses, Reply};
use isothrum_runner::{FakePorts, Runner, RunnerConfig};

use crate::args::Cli;
use crate::commands;
use crate::error::{Error, Result};
use crate::exit::Verdict;
use crate::term::Term;
use crate::testing::Buf;

struct Setup {
    home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    term: Term,
    out: Buf,
}

/// A Compose project in a folder of its own, and a runner whose tools answer nothing.
fn setup() -> Setup {
    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(run.path().join("compose.yaml"), "services:\n  web:\n    image: nginx\n").unwrap();
    let fake = Arc::new(FakeProcesses::new(|_| Reply::ok("")));
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::<FakeProcesses>::clone(&fake);
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
    /// `isothrum <args> <run>`: the command's verdict.
    async fn run(&self, args: &[&str]) -> Result<Verdict> {
        let run = self.run.path().display().to_string();
        let mut command_line = vec!["isothrum"];
        command_line.extend_from_slice(args);
        command_line.push(&run);
        let cli = Cli::parse_from(command_line);
        commands::run(&cli, || Ok(&self.runner), &self.term, &crate::term::Trail::default(), &|_| None).await
    }
}

#[tokio::test]
async fn completions_build_no_runner_so_touch_no_home() {
    let out = Buf::default();
    let term = Term::new(Box::new(out.clone()), Box::new(Buf::default()));
    let cli = Cli::parse_from(["isothrum", "completions", "bash"]);
    let no_runner = || -> Result<Runner> { panic!("completions built a runner") };
    let verdict = commands::run(&cli, no_runner, &term, &crate::term::Trail::default(), &|_| None).await;
    assert_eq!(verdict.unwrap(), Verdict::Pass);
    assert!(out.take().contains("isothrum"), "the script");
}

/// Regression: a malformed `--name` exited 1 with the code `run_name`, where the exit-code
/// reference promises 2 for a malformed value.
#[tokio::test]
async fn a_malformed_name_is_a_usage_error() {
    let s = setup();
    let err = s.run(&["status", "--name", "Not A Name"]).await.unwrap_err();
    assert!(matches!(&err, Error::Usage(why) if why.starts_with("--name: ")), "{err}");
    assert_eq!(crate::exit::code(&Err(err)), std::process::ExitCode::from(crate::exit::USAGE));
}

#[tokio::test]
async fn key_prints_the_public_key_or_with_json_one_document() {
    // Regression: `key` ignored `--json`, printing the bare key a script reading JSON can't parse.
    let s = setup();
    let ssh = s.home.path().join("ssh");
    std::fs::create_dir_all(&ssh).unwrap();
    std::fs::write(ssh.join("id_ed25519"), "private").unwrap();
    std::fs::write(ssh.join("id_ed25519.pub"), "ssh-ed25519 AAAAfake isothrum\n").unwrap();
    for (json, said) in [
        (false, "ssh-ed25519 AAAAfake isothrum\n"),
        (true, "{\"public_key\":\"ssh-ed25519 AAAAfake isothrum\"}\n"),
    ] {
        let line: &[&str] = if json { &["isothrum", "key", "--json"] } else { &["isothrum", "key"] };
        let cli = Cli::parse_from(line);
        let verdict = commands::run(&cli, || Ok(&s.runner), &s.term, &crate::term::Trail::default(), &|_| None).await;
        assert_eq!(verdict.unwrap(), Verdict::Pass);
        assert_eq!(s.out.take(), said);
    }
}
