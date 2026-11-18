//! Projects run for real on this computer through Isothrum's runner, as an embedder runs them (the
//! launcher, `isothrum up`): the Compose example on the engine the Docker CLI points at, and the
//! Vagrant example on each hypervisor ready here, with the box the hypervisor self-test uses. Each
//! goes up, reads ready with every machine running, and goes down again, absent; each skips,
//! saying why, where its engine or hypervisor isn't ready. What a test brought up it takes down,
//! also when it fails; the box it fetched stays, as `vagrant up` leaves it.

use std::path::PathBuf;
use std::sync::Arc;

use isothrum_machine::docker;
use isothrum_machine::providers::{self, Provider};
use isothrum_machine::selftest::hypervisor::box_for;
use isothrum_runner::{Event, MachineState, RunName, RunOptions, RunState, Runner, RunnerConfig};

use crate::here;

/// One of the repository's examples.
fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(name)
}

/// Each event as its line, so the log shows what the run did.
fn said(e: &Event) {
    println!("{}", serde_json::to_string(e).unwrap_or_default());
}

/// The project in `dir` up through the runner (on a home of its own, its tools this computer's
/// as the launcher finds them), ready with every machine running, then down and absent; down
/// also when up or the checks fail, and the first failure is the test's.
async fn up_ready_down(dir: PathBuf, opts: RunOptions) {
    let cx = here().await;
    let home = tempfile::tempdir().expect("a home");
    let mut config = RunnerConfig::new(home.path());
    config.processes = Arc::clone(&cx.runner);
    let runner = Runner::new(config).expect("a runner");
    let run = runner
        .open(&dir, Some(RunName::new("isothrum-e2e-run").expect("a name")))
        .expect("the example opens");
    let up = run.up(&opts, &said).await;
    let ready = match &up {
        Ok(_) => run.status_with(&opts).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    let down = run.down(&opts, &said).await;
    let status = ready.unwrap_or_else(|why| panic!("up: {why}"));
    println!("{status:#?}");
    assert_eq!(status.state, RunState::Ready, "the run reads ready");
    assert!(!status.machines.is_empty(), "it has machines");
    for m in &status.machines {
        assert_eq!(m.state, MachineState::Running, "{} runs", m.name);
    }
    down.unwrap_or_else(|why| panic!("down: {why}"));
    assert_eq!(run.status().await.expect("its status").state, RunState::Absent, "taken down");
}

/// The Vagrant example up on `provider`, with the box the hypervisor self-test uses there, when
/// the provider is ready here; skips, saying why, else.
async fn vagrant_example_on(provider: Provider) {
    let cx = here().await;
    let Some(status) = providers::status(&cx, provider).await else {
        skip!("{} isn't offered here", provider.id())
    };
    if !status.available {
        skip!("{} isn't ready here: {:?} (needs {:?})", provider.id(), status.reason, status.needs)
    }
    let Some((vm_box, _)) = box_for(&cx, provider).await else {
        skip!("no test box exists for {} on this computer's CPU ({:?})", provider.id(), cx.host.arch)
    };
    println!("the box: {vm_box}");
    up_ready_down(example("vagrant-one-vm"), RunOptions::new().with_provider(provider).with_env("BOX", vm_box)).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_compose_example_runs_here() {
    let cx = here().await;
    if let Err(why) = docker::daemon_os(&cx).await {
        skip!("no container engine answers here: {why}")
    }
    up_ready_down(example("compose-web-cache"), RunOptions::new()).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_virtualbox() {
    vagrant_example_on(Provider::Virtualbox).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_hyperv() {
    vagrant_example_on(Provider::Hyperv).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_vmware_desktop() {
    vagrant_example_on(Provider::VmwareDesktop).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_parallels() {
    vagrant_example_on(Provider::Parallels).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_libvirt() {
    vagrant_example_on(Provider::Libvirt).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_qemu() {
    vagrant_example_on(Provider::Qemu).await;
}

#[tokio::test]
#[ignore = "real machine: cargo xtask e2e --tier machine"]
async fn the_vagrant_example_runs_on_utm() {
    vagrant_example_on(Provider::Utm).await;
}
