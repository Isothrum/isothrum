//! `reap` end to end on a runner whose tools are scripted: a cloud run started with no life
//! left, a fake Terraform, and a fake `aws` listing what the account holds.

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{FakePorts, Runner, RunnerConfig};

use crate::args::Cli;
use crate::commands;
use crate::exit::Verdict;
use crate::term::Term;
use crate::testing::Buf;

const SOLO_WEB_AWS: &str = include_str!("../../../../isothrum-runner/tests/fixtures/solo-web.cloud-vm-aws.run.json");

/// What the account holds: an instance nobody records, past its expiry.
const TAGGED: &str = r#"{"ResourceTagMappingList": [{"ResourceARN": "arn:aws:ec2:eu-west-3:1:instance/i-0lost", "Tags": [
  {"Key": "isoloom-expires-at", "Value": "1000"}, {"Key": "isoloom-instance", "Value": "isoloom-solo-web-0lost000"}]}]}"#;

/// The run's state folder the fake Terraform writes in.
fn state_dir(call: &Call) -> std::path::PathBuf {
    Path::new(call.env("TF_DATA_DIR").unwrap()).parent().unwrap().to_path_buf()
}

fn answer(home: &Path, call: &Call) -> Reply {
    let line = call.line();
    match call.program.as_str() {
        "ssh-keygen" => {
            let key = home.join("ssh").join("id_ed25519");
            std::fs::write(&key, "k").unwrap();
            std::fs::write(key.with_extension("pub"), "ssh-ed25519 AAAA isothrum\n").unwrap();
            Reply::ok("")
        }
        "terraform" if line.starts_with("terraform apply") => {
            let state = r#"{"outputs":{"address_web":{"value":"198.51.100.10"},"ssh_user":{"value":"admin"},"ready_file":{"value":"/var/lib/isoloom/ready"}},"resources":[{"instances":[{}]}]}"#;
            std::fs::write(state_dir(call).join("terraform.tfstate"), state).unwrap();
            Reply::ok("")
        }
        "terraform" if line.starts_with("terraform destroy") => {
            std::fs::write(state_dir(call).join("terraform.tfstate"), r#"{"resources":[]}"#).unwrap();
            Reply::ok("")
        }
        "ssh" if line.contains("head -n 1") => Reply::ok("ready\n--isoloom-log--\n"),
        "aws" => Reply::ok(TAGGED),
        _ => Reply::ok(""),
    }
}

/// [`answer`], but `terraform destroy` leaves the home a run record that isn't JSON, so the search
/// that follows the sweep fails (`io`); it succeeds when `destroyed`.
fn destroy_breaking_a_record(home: &Path, call: &Call, destroyed: bool) -> Reply {
    if call.line().starts_with("terraform destroy") {
        let other = home.join("runs/other");
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(other.join("record.json"), "not json").unwrap();
        if !destroyed {
            return Reply::fail(1, "Error: destroy failed");
        }
    }
    answer(home, call)
}

struct Setup {
    _home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    fake: Arc<FakeProcesses>,
    term: Term,
    out: Buf,
    err: Buf,
}

fn setup() -> Setup {
    setup_answering(answer)
}

/// [`setup`], the tools answered by `answer`.
fn setup_answering(answer: fn(&Path, &Call) -> Reply) -> Setup {
    let (home, run) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let dir = run.path().join(".isoloom/cloud-vm/aws");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("run.json"), SOLO_WEB_AWS).unwrap();
    std::fs::write(dir.join("main.tf"), "").unwrap();
    let fake = Arc::new(FakeProcesses::new({
        let home = home.path().to_path_buf();
        move |call| answer(&home, call)
    }));
    let mut config = RunnerConfig::new(home.path());
    let processes: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
    config.processes = processes;
    config.ports = Arc::new(FakePorts::new([]));
    let (out, err) = (Buf::default(), Buf::default());
    let term = Term::new(Box::new(out.clone()), Box::new(err.clone()));
    Setup {
        _home: home,
        runner: Runner::new(config).unwrap(),
        run,
        fake,
        term,
        out,
        err,
    }
}

impl Setup {
    async fn run(&self, args: &[&str]) -> Verdict {
        let cli = Cli::parse_from(std::iter::once("isothrum").chain(args.iter().copied()));
        commands::run(&cli, || Ok(&self.runner), &self.term, &crate::term::Trail::default(), &|_| None)
            .await
            .unwrap()
    }

    /// The run started with no life left: expired at once.
    async fn expired_run(&self) {
        let run = self.run.path().display().to_string();
        let up = [
            "up",
            run.as_str(),
            "--target",
            "cloud-vm/aws",
            "--param",
            "allowed_cidr=203.0.113.7/32",
            "--lifetime",
            "0",
        ];
        assert_eq!(self.run(&up).await, Verdict::Pass);
        self.out.take();
    }
}

#[tokio::test]
async fn reap_destroys_a_clouds_expired_runs_and_lists_what_no_run_records() {
    let s = setup();
    s.expired_run().await;
    let verdict = s.run(&["reap", "aws", "--region", "eu-west-3", "--json"]).await;
    let said = s.out.take();
    let events: Vec<serde_json::Value> = said.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert!(events.iter().all(|e| e["v"] == 0 && e["event"].is_string()), "one event per line: {said}");
    let last = events.last().unwrap();
    assert_eq!((last["event"].as_str(), last["ok"].as_bool()), (Some("result"), Some(false)), "{last}");
    assert!(last.get("run").is_none() && last.get("op").is_none(), "no one run's operation: {last}");
    let summary = &last["data"];
    assert_eq!(summary["destroyed"], serde_json::json!(["solo-web"]));
    assert!(s.fake.last("terraform destroy").is_some());
    assert_eq!(summary["orphans"][0]["id"], "arn:aws:ec2:eu-west-3:1:instance/i-0lost");
    assert_eq!(verdict, Verdict::Fail, "something is left");
    assert!(
        s.fake
            .last("aws resourcegroupstaggingapi get-resources")
            .unwrap()
            .line()
            .ends_with("--region eu-west-3")
    );
}

#[tokio::test]
async fn a_dry_run_destroys_nothing() {
    let s = setup();
    s.expired_run().await;
    let verdict = s.run(&["reap", "aws", "--dry-run"]).await;
    assert_eq!(verdict, Verdict::Fail, "something is left");
    let said = s.out.take();
    assert!(said.contains("would destroy solo-web (--dry-run)"), "{said}");
    assert!(said.contains("left ec2 instance arn:aws:ec2:eu-west-3:1:instance/i-0lost"), "{said}");
    assert!(s.fake.last("terraform destroy").is_none());
}

#[tokio::test]
async fn another_clouds_runs_are_left_alone() {
    let s = setup();
    s.expired_run().await;
    let verdict = s.run(&["reap", "linode"]).await;
    assert_eq!(verdict, Verdict::Fail, "a query that couldn't run is no clean sweep");
    assert!(s.fake.last("terraform destroy").is_none());
    let said = s.out.take();
    assert!(
        said.contains("not checked: `linode-cli linodes list --json`"),
        "an answer it can't read: {said}"
    );
}

#[tokio::test]
async fn a_search_that_fails_after_a_failed_destroy_still_says_the_sweep() {
    // Regression: the search's `io` was returned, and the command line took the failed destroy's
    // `error` for having said it: nothing was printed, not even the sweep.
    let s = setup_answering(|home, call| destroy_breaking_a_record(home, call, false));
    s.expired_run().await;
    let verdict = s.run(&["reap", "aws"]).await;
    assert_eq!(verdict, Verdict::Fail);
    let (said, errors) = (s.out.take(), s.err.take());
    assert!(said.starts_with("failed solo-web: "), "{said}");
    assert!(!said.contains("nothing to reap"), "the cloud wasn't searched: {said}");
    let last = errors.lines().last().unwrap();
    assert!(
        last.starts_with("error: ") && last.ends_with("record.json couldn't be read: expected ident at line 1 column 2"),
        "{errors}"
    );
}

#[tokio::test]
async fn a_search_that_fails_after_a_destroy_is_no_runs_error() {
    // Regression: the search's `error` event carried the destroyed run's `run` and `op`.
    let s = setup_answering(|home, call| destroy_breaking_a_record(home, call, true));
    s.expired_run().await;
    let verdict = s.run(&["reap", "aws", "--json"]).await;
    assert_eq!(verdict, Verdict::Fail);
    let said = s.out.take();
    let events: Vec<serde_json::Value> = said.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let [.., result, error] = events.as_slice() else {
        panic!("a result, then an error: {said}");
    };
    assert_eq!((result["event"].as_str(), result["ok"].as_bool()), (Some("result"), Some(false)), "{result}");
    assert_eq!(result["data"]["destroyed"], serde_json::json!(["solo-web"]));
    assert!(
        result["data"]["orphans"].is_null() && result["data"]["unread"].is_null(),
        "not searched: {result}"
    );
    assert_eq!(
        (error["event"].as_str(), error["code"].as_str(), error["seq"].as_u64()),
        (Some("error"), Some("io"), Some(2))
    );
    assert!(error.get("run").is_none() && error.get("op").is_none(), "no one run's operation: {error}");
}
