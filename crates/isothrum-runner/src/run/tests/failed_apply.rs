//! A Terraform run whose apply failed after making part of the module (#113): its state holds an
//! instance, untainted, made before the one that failed. A status must not read it ready, or the
//! next `up` is refused as `already_running`; the next `up` applies again, and once that ends
//! well the run is ready.

use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::formats::contract::Model;
use crate::formats::terraform::tests::contract::LibvirtModule;
use crate::process::fake::FakeProcesses;

/// What Terraform says of the instance it couldn't make, as `apply -json` prints an error.
const FAILED: &str = r#"{"@level":"error","@message":"Error: creating libvirt network: the network's bridge is taken","type":"diagnostic","diagnostic":{"severity":"error","summary":"creating libvirt network: the network's bridge is taken","detail":""}}"#;

/// The module, whose first apply makes what it can and then fails.
struct FailsFirst {
    module: LibvirtModule,
    failed: AtomicBool,
}

impl FailsFirst {
    fn answer(&self, call: &Call) -> Reply {
        let reply = self.module.answer(call);
        let apply = call.args.first().is_some_and(|a| a == "apply");
        if apply && !self.failed.swap(true, Ordering::SeqCst) {
            return Reply::fail(1, FAILED);
        }
        reply
    }
}

#[tokio::test]
async fn a_failed_apply_leaves_the_run_failed_whatever_its_state_holds_and_the_next_up_applies_again() {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("lab");
    std::fs::create_dir(&project).expect("the module");
    std::fs::write(project.join(LibvirtModule::FILE.0), LibvirtModule::FILE.1).expect("a module");
    let world = Arc::new(FailsFirst {
        module: LibvirtModule::new(false),
        failed: AtomicBool::new(false),
    });
    let fake = Arc::new(FakeProcesses::model({
        let world = Arc::clone(&world);
        move |call| world.answer(call)
    }));
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    let run = runner.open(&project, None).expect("opened");

    run.up(&RunOptions::new(), &Recorder::new()).await.expect_err("the apply failed");
    let status = run.status().await.expect("status");
    assert!(!status.machines.is_empty(), "the state holds what the apply made: {status:?}");
    assert_eq!(status.state, RunState::Failed, "what a failed apply left proves nothing ready");
    assert_eq!(run.status().await.expect("status").state, RunState::Failed, "and stays so");

    run.up(&RunOptions::new(), &Recorder::new())
        .await
        .expect("an up applies again, never already_running");
    assert_eq!(run.status().await.expect("status").state, RunState::Ready);
}
