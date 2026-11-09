//! `reap` end to end on a runner whose tools are scripted: a cloud run started with no life
//! left, a fake Terraform, and a fake `aws` listing what the account holds.

use std::path::Path;
use std::sync::Arc;

use clap::Parser;
use isothrum_runner::process::fake::{Call, FakeProcesses, Reply};
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

struct Setup {
    _home: tempfile::TempDir,
    run: tempfile::TempDir,
    runner: Runner,
    fake: Arc<FakeProcesses>,
    term: Term,
    out: Buf,
}

fn setup() -> Setup {
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
    let out = Buf::default();
    let term = Term::new(Box::new(out.clone()), Box::new(Buf::default()));
    Setup {
        _home: home,
        runner: Runner::new(config).unwrap(),
        run,
        fake,
        term,
        out,
    }
}

impl Setup {
    async fn run(&self, args: &[&str]) -> Verdict {
        let cli = Cli::parse_from(std::iter::once("isothrum").chain(args.iter().copied()));
        commands::run(&cli, &self.runner, &self.term, &|_| None).await.unwrap()
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
    let summary: serde_json::Value = serde_json::from_str(s.out.take().lines().last().unwrap()).unwrap();
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
