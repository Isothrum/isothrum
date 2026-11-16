//! A status read gives a sensitive output's own value only when it is asked to reveal it.

use super::*;
use crate::formats::contract::Model;
use crate::formats::terraform::tests::contract::LibvirtModule;
use crate::process::fake::FakeProcesses;

#[tokio::test]
async fn a_sensitive_output_is_masked_unless_the_status_read_reveals_it() {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("stack");
    std::fs::create_dir(&project).expect("the module");
    std::fs::write(project.join("main.tf"), "output \"token\" {\n  value = \"x\"\n  sensitive = true\n}\n").expect("a module");
    let module = Arc::new(LibvirtModule::new(false));
    let fake = Arc::new(FakeProcesses::model({
        let module = Arc::clone(&module);
        move |call| module.answer(call)
    }));
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    let run = runner.open(&project, None).expect("opened");
    let events = Recorder::new();
    run.up(&RunOptions::new(), &events).await.expect("up");

    let token = |status: &crate::Status| status.outputs.iter().find(|o| o.name == "token").and_then(|o| o.value.clone());
    let masked = run.status().await.expect("status");
    assert_eq!(token(&masked).as_deref(), Some("***"), "masked by default");
    let revealed = run.status_with(&RunOptions::new().with_reveal()).await.expect("status");
    assert_eq!(token(&revealed).as_deref(), Some("s3cretTOKEN"), "its own value when asked");
    assert!(
        revealed.outputs.iter().find(|o| o.name == "token").is_some_and(|o| o.sensitive),
        "still said sensitive"
    );
    let json = serde_json::to_string(&events.events()).expect("json");
    assert!(!json.contains("s3cretTOKEN"), "never in an event: {json}");
}
