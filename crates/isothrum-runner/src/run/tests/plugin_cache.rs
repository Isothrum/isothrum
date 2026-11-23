//! The provider cache a Terraform run's tools get: the user's own (`TF_PLUGIN_CACHE_DIR`) when
//! set, made when it isn't there yet (an owner's decision: Terraform refuses a cache folder that
//! doesn't exist), else the one every run of the runner shares.

use super::*;
use crate::process::fake::FakeProcesses;

#[tokio::test]
async fn the_users_own_plugin_cache_is_used_and_made_when_it_isnt_there() {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("module");
    std::fs::create_dir(&project).expect("the module");
    std::fs::write(project.join("main.tf"), "resource \"terraform_data\" \"web\" {}\n").expect("main.tf");
    let users = root.path().join("users-own/plugins");
    let fake = Arc::new(FakeProcesses::script().build());
    let mut config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    config.tf_plugin_cache = Some(users.clone());
    let runner = Runner::new(config).expect("a runner");
    let run = runner.open(&project, None).expect("opened");

    // A status reads the state, which no apply made yet: nothing runs, but the tools' environment
    // is set up, the cache with it.
    assert_eq!(run.status().await.expect("status").state, RunState::Absent);
    assert!(users.is_dir(), "the user's own cache, made");
    assert!(!root.path().join("home/terraform-plugins").exists(), "never the shared one beside it");
}
