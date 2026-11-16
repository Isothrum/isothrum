//! A deployment's published service endpoints, as this computer reaches them.

use super::*;
use crate::process::fake::{FakeProcesses, Reply};

/// A Terraform run whose state holds one VM and whose `published` output is `published`; the
/// runner and its project.
fn published(published: &str) -> (tempfile::TempDir, Runner, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("module");
    std::fs::create_dir(&project).expect("the module");
    std::fs::write(project.join("main.tf"), "resource \"terraform_data\" \"web\" {}\n").expect("a module");
    let outputs =
        serde_json::json!({ "published": { "sensitive": false, "type": "map", "value": serde_json::from_str::<serde_json::Value>(published).expect("json") } })
            .to_string();
    let fake = Arc::new(
        FakeProcesses::script()
            .allow("terraform show -json", Reply::ok(r#"{"format_version":"1.0"}"#))
            .allow("terraform output -json", Reply::ok(&outputs))
            .build(),
    );
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    let state = root.path().join("home").join("runs").join("module");
    std::fs::create_dir_all(&state).expect("the run's state folder");
    std::fs::write(state.join("terraform.tfstate"), "{}").expect("a state");
    (root, runner, project)
}

#[tokio::test]
async fn without_a_jump_host_each_endpoint_is_where_the_deployment_publishes_it() {
    let (_root, runner, project) = published(r#"{"web/site":"203.0.113.7:8080","web/admin":":9090"}"#);
    let run = runner.open(&project, None).expect("opened");
    let endpoints = run.service_endpoints(&RunOptions::new()).await.expect("endpoints");
    let reached: Vec<(&str, Option<&str>, bool)> = endpoints.iter().map(|e| (e.name.as_str(), e.address.as_deref(), e.forwarded)).collect();
    assert_eq!(
        reached,
        [("web/site", Some("203.0.113.7:8080"), false)],
        "one without its address yet is left out"
    );
}

#[tokio::test]
async fn a_jump_host_that_isnt_a_login_is_refused() {
    let (_root, runner, project) = published(r#"{"web/site":"192.0.2.10:8080"}"#);
    let run = runner.open(&project, None).expect("opened");
    let e = run
        .service_endpoints(&RunOptions::new().with_env("ssh_jump", "node.stack"))
        .await
        .expect_err("no user");
    assert_eq!(e.code(), crate::ErrorCode::InputInvalid, "{e}");
}
