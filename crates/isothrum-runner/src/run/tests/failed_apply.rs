//! A Terraform run whose apply failed after making part of the module (#113): its state holds an
//! instance, untainted, made before the one that failed. A status must not read it ready, or the
//! next `up` is refused as `already_running`; the next `up` applies again, and once that ends
//! well the run is ready. An operation Terraform doesn't do, refused before acting, leaves the run
//! as it was.

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

/// A runner whose Terraform answers as `answer` does, and the module's folder, in a temporary
/// folder.
fn module_runner(answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> (tempfile::TempDir, std::path::PathBuf, Runner) {
    let root = tempfile::tempdir().expect("a folder");
    let project = root.path().join("stack");
    std::fs::create_dir(&project).expect("the module");
    std::fs::write(project.join(LibvirtModule::FILE.0), LibvirtModule::FILE.1).expect("a module");
    let fake = Arc::new(FakeProcesses::model(answer));
    let config = runner_config(&root.path().join("home"), processes(&fake), no_ports(), crate::runner::this_host());
    let runner = Runner::new(config).expect("a runner");
    (root, project, runner)
}

#[tokio::test]
async fn a_failed_apply_leaves_the_run_failed_whatever_its_state_holds_and_the_next_up_applies_again() {
    let world = FailsFirst {
        module: LibvirtModule::new(false),
        failed: AtomicBool::new(false),
    };
    let (_root, project, runner) = module_runner(move |call| world.answer(call));
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

/// A park, a resume or a provision Terraform doesn't do is refused before anything acts: the run
/// stays ready, and wanted running, as it was, never `failed` (an `up` was needed to bring it
/// back).
#[tokio::test]
async fn an_operation_terraform_declines_leaves_the_run_as_it_was() {
    let module = LibvirtModule::new(false);
    let (_root, project, runner) = module_runner(move |call| module.answer(call));
    let run = runner.open(&project, None).expect("opened");
    run.up(&RunOptions::new(), &Recorder::new()).await.expect("up");
    for declined in [
        run.park(crate::Park::Pause, &RunOptions::new(), &Recorder::new()).await,
        run.resume(&RunOptions::new(), &Recorder::new()).await,
        run.provision(None, None, &RunOptions::new(), &Recorder::new()).await,
    ] {
        let e = declined.expect_err("declined");
        assert_eq!((e.code(), e.report().changed), (ErrorCode::Unsupported, false), "{e}");
        let row = runner.store.db().read(|tx| tx.run(run.name())).expect("the store").expect("the run");
        assert_eq!((row.state, row.desired), (RunState::Ready, crate::Desired::Running), "{e}");
    }
}
