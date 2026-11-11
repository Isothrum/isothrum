//! What a user does to a VM run outside the runner: a VM deleted (`vagrant destroy` in a terminal,
//! the hypervisor's own Delete) or powered off (the hypervisor's window closed, `virsh destroy`,
//! VirtualBox's Power Off) while the run runs or while its start provisions it, Vagrant
//! uninstalled, the process running the start killed. The run's status must say what is there,
//! a resume must never make a deleted VM again unprovisioned, and the way back (a `down`, or the
//! next `up` clearing what is left) must work.

use super::*;
use crate::status::MachineState;

/// `machine`'s state in `status`.
fn machine(status: &crate::Status, name: &str) -> MachineState {
    status.machines.iter().find(|m| m.name == name).map(|m| m.state.clone()).unwrap()
}

/// The fixture's run, started.
async fn started(f: &Fixture) -> crate::Running<'_> {
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    run
}

/// `run` taken down: its VMs gone, the run absent.
async fn goes(f: &Fixture, run: &crate::Running<'_>) {
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(
        (f.state("web"), run.status().await.unwrap().state),
        ("not_created".to_owned(), RunState::Absent)
    );
}

/// Whether a Vagrant call boots `vm` without provisioning it.
fn booted_unprovisioned(f: &Fixture, vm: &str) -> bool {
    f.vagrant()
        .iter()
        .any(|l| l.contains("--no-provision") && l.contains(vm) && l.starts_with("vagrant up"))
}

#[tokio::test]
async fn a_vm_deleted_outside_shows_gone_and_a_resume_never_makes_it_again_unprovisioned() {
    let f = fixture(crate::runner::this_host());
    let run = started(&f).await;
    f.engine().states.remove("web");

    let status = run.status().await.unwrap();
    assert_eq!((status.state, machine(&status, "web")), (RunState::Failed, MachineState::Absent));

    crate::testing::refuses_to_resume_without(&run, "web").await;
    assert!(
        !booted_unprovisioned(&f, "web"),
        "a bare VM from the box would read as ready: {:?}",
        f.vagrant()
    );
    assert_eq!(f.state("web"), "not_created");

    // The next start clears what is left (the controller), then builds the run again.
    let again = Recorder::new();
    run.up(&opts(), &again).await.unwrap();
    let said = serde_json::to_string(&again.events()).unwrap();
    assert!(said.contains("leftovers_removed"), "{said}");
    assert!(f.vagrant().contains(&"vagrant destroy --force".to_owned()), "{:?}", f.vagrant());
    assert_eq!(f.state("web"), "running");
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
}

#[tokio::test]
async fn a_vm_powered_off_outside_shows_off_and_a_resume_boots_it_again() {
    let f = fixture(crate::runner::this_host());
    let run = started(&f).await;
    // VirtualBox's Power Off, a closed hypervisor window: the VM is there, off.
    f.engine().states.insert("web".into(), "poweroff".into());

    let status = run.status().await.unwrap();
    assert_eq!((status.state, machine(&status, "web")), (RunState::Failed, MachineState::Stopped));
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "running");
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
}

#[tokio::test]
async fn a_vm_deleted_while_the_start_provisions_it_fails_the_start_and_the_next_one_clears_it() {
    let f = fixture(crate::runner::this_host());
    f.engine().ups = vec![("The VM 'web' couldn't be found: it was deleted while it booted.".into(), true)];
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    // The controller came up before `web` was deleted.
    f.engine().states.insert(CONTROLLER.into(), "running".into());
    assert_eq!(err.code(), ErrorCode::ToolFailed);
    assert!(err.to_string().contains("couldn't be found"), "Vagrant's words: {err}");
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Failed);
    assert_eq!(run.status().await.unwrap().state, RunState::Failed);

    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(f.vagrant().contains(&"vagrant destroy --force".to_owned()), "{:?}", f.vagrant());
    assert_eq!((f.state("web").as_str(), f.state(CONTROLLER).as_str()), ("running", "poweroff"));
}

#[tokio::test]
async fn a_start_killed_while_it_provisions_is_failed_and_the_next_start_clears_it() {
    let f = fixture(crate::runner::this_host());
    let run = started(&f).await;
    // The process running the start was killed: its record stays `starting`, its VMs up.
    let mut record = f.runner.store.record(run.name()).unwrap().unwrap();
    record.state = RunState::Starting;
    f.runner.store.save(&record).unwrap();

    assert_eq!(run.status().await.unwrap().state, RunState::Failed);
    let ups = f.vagrant().iter().filter(|l| l.starts_with("vagrant up --provider")).count();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(f.vagrant().contains(&"vagrant destroy --force".to_owned()), "{:?}", f.vagrant());
    assert_eq!(f.vagrant().iter().filter(|l| l.starts_with("vagrant up --provider")).count(), ups + 1);
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Ready);
}

#[tokio::test]
async fn vagrant_uninstalled_mid_run_refuses_and_a_down_once_it_is_back_still_works() {
    let f = fixture(crate::runner::this_host());
    let run = started(&f).await;
    let mut config = f.runner.config.clone();
    config.processes = Arc::new(FakeProcesses::rules(&[("vagrant", Reply::missing())]));
    let without = Runner::new(config).unwrap();
    let gone = without.open(&f.manifest, None).unwrap();
    assert_eq!(gone.status().await.unwrap_err().code(), ErrorCode::ToolMissing);
    assert_eq!(
        gone.down(&RunOptions::new(), &Recorder::new()).await.unwrap_err().code(),
        ErrorCode::ToolMissing
    );
    assert!(!f.runner.store.record(run.name()).unwrap().unwrap().state.is_moving());
    assert_eq!(f.state("web"), "running", "nothing was removed");

    goes(&f, &run).await;
}

#[tokio::test]
async fn a_down_of_a_run_whose_vms_were_all_deleted_outside_ends_absent() {
    let f = fixture(crate::runner::this_host());
    let run = started(&f).await;
    f.engine().states.clear();
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.runner.store.record(run.name()).unwrap().unwrap().state, RunState::Absent);
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "running");
}
