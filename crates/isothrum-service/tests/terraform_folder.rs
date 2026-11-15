//! Any Terraform folder through the runner, the service and the client: the cloud-bucket module
//! (`fixtures/cloud-bucket/module`, a public S3 website written by hand, no generator's), its
//! input variables listed, run with a value and a secret, its outputs read back, and removed.
//! Terraform is a model answering with what Terraform 1.16 prints for this module
//! (`fixtures/cloud-bucket/terraform-1.16`): no test reaches AWS.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use isothrum_client::proto::{Command, OpOptions, Target};
use isothrum_client::{Client, ClientError, InputVariable};
use isothrum_runner::fake::{Call, Reply};
use isothrum_runner::{ErrorCode, EventKind, ResultData, RunName, RunState};

mod common;
use common::Outcome;

/// The fixture's folder.
fn fixture(part: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/cloud-bucket").join(part)
}

/// What Terraform 1.16 printed for the module, by file name.
#[expect(clippy::panic, reason = "a fixture that can't be read fails the test that wants it")]
fn recorded(name: &str) -> String {
    std::fs::read_to_string(fixture("terraform-1.16").join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Terraform for one module, as a model: `apply` makes the recorded state (and writes its file
/// where `-state=` says), `destroy` removes it,
/// `show` and `output` read whichever is there. Every call is kept.
#[derive(Default)]
struct Terraform {
    applied: AtomicBool,
    calls: Mutex<Vec<Call>>,
}

impl Terraform {
    #[expect(
        clippy::panic,
        clippy::unwrap_used,
        reason = "the model fails the test on a call no Terraform of this module would answer"
    )]
    fn answer(&self, call: &Call) -> Reply {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).push(call.clone());
        assert_eq!(call.program, "terraform", "only Terraform runs: {}", call.line());
        let applied = self.applied.load(Ordering::SeqCst);
        match call.args.first().map(String::as_str) {
            Some("version") => Reply::ok(&recorded("version.json")),
            Some("init") => Reply::ok("Terraform has been successfully initialized!"),
            Some("apply") => {
                // Terraform writes the state where it was told.
                if let Some(state) = call.args.iter().find_map(|a| a.strip_prefix("-state=")) {
                    std::fs::write(state, "{}").unwrap();
                }
                self.applied.store(true, Ordering::SeqCst);
                Reply::ok(&recorded("apply.jsonl"))
            }
            Some("destroy") => {
                self.applied.store(false, Ordering::SeqCst);
                Reply::ok(&recorded("destroy.jsonl"))
            }
            Some("show") if applied => Reply::ok(&recorded("show.json")),
            Some("show") => Reply::ok(r#"{"format_version":"1.0"}"#),
            Some("output") if applied => Reply::ok(&recorded("output.json")),
            Some("output") => Reply::ok("{}"),
            _ => panic!("a call this module's Terraform doesn't answer: {}", call.line()),
        }
    }

    fn calls(&self, verb: &str) -> Vec<Call> {
        let calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        calls.iter().filter(|c| c.args.first().is_some_and(|a| a == verb)).cloned().collect()
    }
}

/// The files under `dir` whose bytes hold `needle`.
#[expect(clippy::unwrap_used, reason = "a home that can't be listed fails the test")]
fn files_holding(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_holding(&path, needle));
        } else if std::fs::read(&path).is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle)) {
            found.push(path);
        }
    }
    found
}

/// A service over a runner whose Terraform is `terraform`, on a fresh home; the module copied to
/// a folder of its own.
struct Lab {
    home: tempfile::TempDir,
    module: tempfile::TempDir,
    terraform: Arc<Terraform>,
    client: Client,
    task: common::Serving,
}

async fn lab() -> Outcome<Lab> {
    let (home, module) = (tempfile::tempdir()?, tempfile::tempdir()?);
    std::fs::copy(fixture("module/main.tf"), module.path().join("main.tf"))?;
    let terraform = Arc::new(Terraform::default());
    let answering = Arc::clone(&terraform);
    let (client, task) = common::serve(home.path(), move |call| answering.answer(call)).await?;
    Ok(Lab {
        home,
        module,
        terraform,
        client,
        task,
    })
}

impl Lab {
    fn target(&self) -> Target {
        Target::new(self.module.path(), RunName::new("cloud-bucket").ok())
    }

    async fn stop(self) {
        drop(self.client.shutdown().await);
        drop(self.task.await);
        drop(self.home);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hand_written_modules_variables_are_listed_from_its_files() {
    let lab = lab().await.unwrap();
    let vars = lab.client.variables(lab.target()).await.unwrap();
    let region: &InputVariable = &vars[1];
    assert_eq!(vars.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), ["isoloom_instance", "region"]);
    assert_eq!(region.type_constraint.as_deref(), Some("string"));
    assert_eq!(region.default, Some(serde_json::json!("us-east-1")));
    assert!(!region.sensitive && !region.required());
    assert_eq!(lab.terraform.calls.lock().unwrap().len(), 0, "listing runs no tool");
    lab.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_folder_that_isnt_terraform_has_its_variables_refused_by_code() {
    let lab = lab().await.unwrap();
    std::fs::remove_file(lab.module.path().join("main.tf")).unwrap();
    std::fs::write(lab.module.path().join("compose.yaml"), "services: {}\n").unwrap();
    let refused = lab.client.variables(lab.target()).await.unwrap_err();
    assert_eq!(refused.code(), ErrorCode::ProjectInvalid, "{refused}");
    assert!(matches!(refused, ClientError::Refused { .. }), "{refused:?}");
    lab.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hand_written_module_runs_with_its_values_gives_its_outputs_and_goes() {
    let lab = lab().await.unwrap();
    let options = OpOptions {
        env: [("region".to_owned(), "eu-west-3".to_owned())].into(),
        secrets: [("AWS_SECRET_ACCESS_KEY".to_owned(), "wJalrXUtnFEMI".to_owned())].into(),
        ..OpOptions::default()
    };
    let up = Command::Up {
        target: lab.target(),
        options: options.clone(),
        lifetime_s: Some(3600),
    };
    let result = lab.client.follow(up).await.unwrap().outcome().await.unwrap();
    let EventKind::Result {
        ok: true,
        data: ResultData::Started { outputs, .. },
    } = &result.kind
    else {
        panic!("not a start: {result:?}");
    };
    let url = "http://isoloom-cloud-bucket-3f9a1c2e.s3-website-us-east-1.amazonaws.com/";
    let started: Vec<(&str, Option<&str>)> = outputs.iter().map(|o| (o.name.as_str(), o.value.as_deref())).collect();
    assert_eq!(started, [("website_url", Some(url))]);

    let apply = lab.terraform.calls("apply").pop().expect("an apply");
    assert_eq!(apply.env("TF_VAR_region"), Some("eu-west-3"), "a value is the module's variable");
    assert_eq!(apply.env("AWS_SECRET_ACCESS_KEY"), Some("wJalrXUtnFEMI"), "a secret reaches the provider");
    assert!(!apply.args.iter().any(|a| a.contains("wJalrXUtnFEMI") || a.starts_with("-var")), "{apply:?}");

    let name = RunName::new("cloud-bucket").unwrap();
    let status = lab.client.status(&name, None).await.unwrap();
    assert_eq!(status.state, RunState::Ready);
    let read: Vec<(&str, bool, Option<&str>)> = status.outputs.iter().map(|o| (o.name.as_str(), o.sensitive, o.value.as_deref())).collect();
    assert_eq!(read, [("website_url", false, Some(url))]);

    // The home keeps the value, never the secret.
    let kept = files_holding(lab.home.path(), b"eu-west-3");
    assert!(!kept.is_empty(), "the value is recorded, to be replayed");
    assert_eq!(files_holding(lab.home.path(), b"wJalrXUtnFEMI"), Vec::<PathBuf>::new(), "no secret in the home");

    // The down names no value: the recorded one is replayed, the secret given again.
    let down = Command::Down {
        target: lab.target(),
        options: OpOptions {
            secrets: options.secrets.clone(),
            ..OpOptions::default()
        },
    };
    lab.client.follow(down).await.unwrap().outcome().await.unwrap();
    let destroy = lab.terraform.calls("destroy").pop().expect("a destroy");
    assert_eq!(destroy.env("TF_VAR_region"), Some("eu-west-3"), "the start's value, replayed");
    assert_eq!(lab.client.status(&name, None).await.unwrap().state, RunState::Absent);
    lab.stop().await;
}
