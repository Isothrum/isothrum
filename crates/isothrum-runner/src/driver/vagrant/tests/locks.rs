//! Another Vagrant process's machine or index lock: a read or a command refused for it is tried
//! again (a command only when it hadn't acted yet), a status during the run's own operation
//! doesn't ask Vagrant, and a lock held past the patience fails with Vagrant's error.

use super::*;

/// Regression: on Hyper-V, reading a VM's state takes its Vagrant lock, and a `vagrant status`
/// that came while another process read the run's VMs (another process running `vagrant
/// global-status --prune`) was refused at once (`MachineActionLockedError`), failing the start.
/// The read is tried again until the lock is free.
#[tokio::test(start_paused = true)]
async fn a_status_read_another_vagrant_process_locked_out_is_read_again() {
    let f = fixture(crate::runner::this_host());
    f.engine().locked_reads = 3;
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(f.engine().locked_reads, 0);
    let status = run.status().await.unwrap();
    assert_eq!(status.state, RunState::Ready);
}

/// A lock held past [`super::lock::PATIENCE`] is no passing read: the start fails with Vagrant's
/// own error, having asked a bounded number of times.
#[tokio::test(start_paused = true)]
async fn a_lock_held_past_the_patience_still_fails_the_read() {
    let f = fixture(crate::runner::this_host());
    f.engine().locked_reads = u32::MAX;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert!(err.to_string().contains("MachineActionLockedError"), "{err}");
    let reads = f.fake.lines().iter().filter(|l| l.starts_with("vagrant status")).count();
    assert!((2..=31).contains(&reads), "{reads} reads");
}

/// Regression: a status read while the run's own start provisioned its VMs (minutes, a
/// `vagrant up` holding each VM's lock all along) was refused by Vagrant
/// (`MachineActionLockedError`), tried again for a minute, then failed: the app polling the run's
/// status from another process than the start's showed the run as unreadable. While an operation
/// holds the run, status doesn't ask Vagrant: the run is starting, its machines as last read.
#[tokio::test(start_paused = true)]
async fn a_status_while_the_runs_own_start_provisions_it_reports_it_starting() {
    let f = fixture(crate::runner::this_host());
    f.engine().up_takes = std::time::Duration::from_secs(600);
    let run = f.runner.open(&f.manifest, None).unwrap();
    // The embedder's app, polling the run from another process than the start's.
    let app = Runner::new(runner_config(
        f.folder.home.path(),
        processes(&f.fake),
        Arc::new(FakePorts::new([])),
        crate::runner::this_host(),
    ))
    .unwrap();
    let watched = app.open(&f.manifest, None).unwrap();
    let statuses = async {
        // Two minutes into the start's `vagrant up`.
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        (watched.status().await, run.status().await)
    };
    let (opts, events) = (opts(), Recorder::new());
    let (up, (elsewhere, here)) = tokio::join!(run.up(&opts, &events), statuses);
    up.unwrap();
    for (who, status) in [("another process", elsewhere), ("this runner", here)] {
        let status = status.unwrap_or_else(|e| panic!("{who}: {e}"));
        assert_eq!((status.state, status.live), (RunState::Starting, false), "{who}");
        // As the start read them before it brought them up.
        assert!(
            status.machines.iter().all(|m| m.state == crate::MachineState::Absent),
            "{who}: {:?}",
            status.machines
        );
        assert_eq!(status.machines.len(), 2, "{who}");
    }
    // Once the start is done, status reads Vagrant again.
    let after = watched.status().await.unwrap();
    assert_eq!((after.state, after.live), (RunState::Ready, true));
}

/// An update keeps the run ready while it runs: a status meanwhile says so, with the machines as
/// last read (running), not Vagrant's refusal while the update's `vagrant up` holds the VMs.
#[tokio::test(start_paused = true)]
async fn a_status_while_an_update_brings_a_vm_up_reports_the_run_ready_as_last_read() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    assert!(run.status().await.unwrap().live);
    std::fs::write(&f.manifest, with_vm_added("extra")).unwrap();
    {
        let mut e = f.engine();
        e.added.push("extra".into());
        e.up_takes = std::time::Duration::from_secs(600);
    }
    let regenerated = f.runner.open(&f.manifest, None).unwrap();
    let status = async {
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        regenerated.status().await
    };
    let (opts, events) = (opts(), Recorder::new());
    let (updated, status) = tokio::join!(regenerated.update(&opts, &events), status);
    updated.unwrap();
    let status = status.unwrap();
    assert_eq!((status.state, status.live), (RunState::Ready, false));
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.state, crate::MachineState::Running);
    assert!(
        status.outputs.iter().any(|o| o.value.is_some()),
        "its outputs from what was read: {:?}",
        status.outputs
    );
}

/// With no operation holding the run, a lock another Vagrant process holds past
/// [`super::lock::QUERY_PATIENCE`] is no failure of a status read: the machine Vagrant names is
/// busy, after a short, bounded wait.
#[tokio::test(start_paused = true)]
async fn a_status_read_locked_out_past_a_query_patience_says_the_machine_busy() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.engine().locked_reads = u32::MAX;
    let reads = |fx: &Fixture| fx.fake.lines().iter().filter(|l| l.starts_with("vagrant status")).count();
    let before = reads(&f);
    let asked = tokio::time::Instant::now();
    let status = run.status().await.unwrap();
    assert!(asked.elapsed() <= super::super::lock::QUERY_PATIENCE + std::time::Duration::from_secs(2));
    assert!((2..=7).contains(&(reads(&f) - before)), "{} reads", reads(&f) - before);
    assert_eq!((status.state, status.live), (RunState::Ready, false));
    let web = status.machines.iter().find(|m| m.name == "web").unwrap();
    assert_eq!(web.state, crate::MachineState::Busy);
}

/// A status read refused while an operation of another process takes the run (it began after
/// the read did, which saw the run free) gives up at once: that operation holds the VMs for as
/// long as it acts. The run is as its record says, the machine Vagrant named busy, the others as
/// last read.
#[tokio::test(start_paused = true)]
async fn a_status_read_refused_as_an_operation_takes_the_run_gives_up_at_once() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.engine().locked_reads = u32::MAX;
    let lock = f.folder.home.path().join("runs/web-vm/lock");
    let reads = |fx: &Fixture| fx.fake.lines().iter().filter(|l| l.starts_with("vagrant status")).count();
    let before = reads(&f);
    let operation = async {
        // Another process's operation takes the run once the read has been refused a few times.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let held = std::fs::OpenOptions::new().read(true).write(true).open(&lock).unwrap();
        held.lock().unwrap();
        held
    };
    let (status, _held) = tokio::join!(run.status(), operation);
    let status = status.unwrap();
    // Refused at 0, 2 and 4 seconds, the run held since 3: not tried again.
    assert_eq!(reads(&f) - before, 3, "gave up once the run was held");
    assert_eq!((status.state, status.live), (RunState::Ready, false));
    let state = |name: &str| status.machines.iter().find(|m| m.name == name).map(|m| m.state.clone());
    assert_eq!(state("web"), Some(crate::MachineState::Busy), "the machine Vagrant named");
    assert_eq!(state(CONTROLLER), Some(crate::MachineState::Stopped), "as last read");
}

/// Regression: on libvirt, the status read that followed the controller's park was refused by
/// Vagrant's machine index lock (`MachineLocked`, another Vagrant process reading the same VMs),
/// which wasn't recognised as a lock: the start failed at once, and its error said only fog's
/// warnings. The read is tried again until the lock is free.
#[tokio::test(start_paused = true)]
async fn a_status_read_after_the_park_another_process_index_locked_is_read_again() {
    let f = starts_without_a_tolerated_step(|e| e.index_locks_after_halt = 3).await;
    assert_eq!(f.engine().index_locked_reads, 0);
    assert_eq!(f.state(CONTROLLER), "poweroff");
}

/// An index lock held past [`super::lock::PATIENCE`] still fails the start, with Vagrant's own
/// error, which it printed on standard output, never fog's warnings.
#[tokio::test(start_paused = true)]
async fn an_index_lock_held_past_the_patience_fails_with_vagrants_error() {
    let f = fixture(crate::runner::this_host());
    f.engine().index_locks_after_halt = u32::MAX;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err().to_string();
    assert!(
        err.starts_with("step `status` failed: Vagrant can't use the requested machine because it is locked! This\nmeans")
            && err.ends_with("Name: isoloom-controller\nProvider: libvirt (Vagrant::Errors::MachineLocked) (exited with code 1)")
            && !err.contains("[fog]"),
        "{err}"
    );
    let reads = f.fake.lines().iter().filter(|l| l.starts_with("vagrant status")).count();
    assert!((3..=32).contains(&reads), "{reads} reads");
}

/// The controller's halt refused for another Vagrant process's lock did nothing: it is tried again
/// rather than waited out as a controller powering itself off.
#[tokio::test(start_paused = true)]
async fn a_controller_halt_another_vagrant_process_locked_out_is_tried_again() {
    let f = starts_without_a_tolerated_step(|e| e.locked_halts = 2).await;
    assert_eq!(f.state(CONTROLLER), "poweroff");
    assert_eq!(
        f.vagrant().iter().filter(|l| *l == "vagrant halt isoloom-controller").count(),
        3,
        "{:?}",
        f.vagrant()
    );
}

/// Whether `events` warn that a step's Vagrant command runs again for another process's lock.
fn warned_retrying(events: &Recorder) -> bool {
    events
        .events()
        .iter()
        .any(|e| matches!(&e.kind, crate::EventKind::Warning { code: crate::WarningCode::Retrying, message } if message.contains("held a machine's lock")))
}

/// How many times the run asked Vagrant for `call` (exactly, arguments included).
fn calls(f: &Fixture, call: &str) -> usize {
    f.vagrant().iter().filter(|l| *l == call).count()
}

/// Regression (friday, libvirt): a `down` whose `vagrant destroy --force` removed the web VM,
/// then was refused by its machine index lock as the VM left Vagrant's index (another Vagrant
/// process reading the run's VMs), failed the stop. A destroy converges: it runs again, and
/// destroys what is left.
#[tokio::test(start_paused = true)]
async fn a_destroy_refused_by_a_machines_index_lock_after_removing_a_vm_runs_again() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let removed = "==> isoloom-controller: Domain is not created. Please run `vagrant up` first.\n\
                   ==> web: Clearing any previously set forwarded ports...\n==> web: Removing domain...";
    let refusal = Refusal::index(1, "web").after(removed, |e| drop(e.states.remove("web")));
    f.engine().refusals.insert("destroy", refusal);
    let events = Recorder::new();
    run.down(&RunOptions::new(), &events).await.unwrap();
    assert_eq!(calls(&f, "vagrant destroy --force"), 2, "{:?}", f.vagrant());
    assert!(f.engine().states.is_empty(), "every VM destroyed: {:?}", f.engine().states);
    assert!(warned_retrying(&events), "{:?}", events.events());
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

/// Regression: a `vagrant up` that another Vagrant process's machine lock refused failed the start
/// (Vagrant's "is locked" even read as an earlier run's leftovers: everything was destroyed and
/// built again). Ten minutes into booting and provisioning the VMs, having acted on them, it is
/// still run again (a second `up` boots only what isn't running, and provisions only a VM whose
/// provisioning hadn't begun), from its own refusal on, not from the start of the step: no
/// recovery, nothing destroyed.
#[tokio::test(start_paused = true)]
async fn an_up_refused_by_a_machines_lock_runs_again_and_is_no_leftover() {
    let f = fixture(crate::runner::this_host());
    let booted = "==> web: Machine booted and ready!\n==> web: Running provisioner: setup (shell)...";
    let refusal = Refusal {
        took: std::time::Duration::from_secs(600),
        ..Refusal::index(1, "isoloom-controller").after(booted, |e| drop(e.states.insert("web".into(), "running".into())))
    };
    f.engine().refusals.insert("up", refusal);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    assert_eq!(calls(&f, "vagrant up --provider virtualbox"), 2, "{:?}", f.vagrant());
    assert_eq!(calls(&f, "vagrant destroy --force"), 0, "no recovery: {:?}", f.vagrant());
    let said = serde_json::to_string(&events.events()).unwrap();
    assert!(!said.contains("recovering"), "{said}");
    assert!(warned_retrying(&events), "{said}");
    assert_eq!(run.status().await.unwrap().state, RunState::Ready);
}

/// A `vagrant provision` another process's action lock refused before it provisioned anything
/// runs again. One refused once it had provisioned a VM (`==> web: Running provisioner: …`) does
/// not: a second run would provision that VM twice.
#[tokio::test(start_paused = true)]
async fn a_provision_refused_before_it_acted_runs_again_never_after() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.engine().refusals.insert("provision", Refusal::action(2, "provision", "web"));
    let events = Recorder::new();
    run.provision(None, &opts(), &events).await.unwrap();
    assert_eq!(calls(&f, "vagrant provision web"), 3, "{:?}", f.vagrant());
    assert!(warned_retrying(&events), "{:?}", events.events());

    let provisioned = "==> web: Running provisioner: setup (shell)...\n    web: Running: script: setup";
    f.engine().refusals.insert("provision", Refusal::index(1, "web").after(provisioned, |_| {}));
    let refused = Recorder::new();
    let err = run.provision(None, &opts(), &refused).await.unwrap_err().to_string();
    assert!(err.contains("Vagrant can't use the requested machine because it is locked!"), "{err}");
    assert_eq!(calls(&f, "vagrant provision web"), 4, "not run again: {:?}", f.vagrant());
    assert!(!warned_retrying(&refused), "{:?}", refused.events());
}

/// A VM's restart for its new boot settings (`vagrant reload`, the driver's own call) refused
/// before it acted runs again.
#[tokio::test(start_paused = true)]
async fn a_restart_refused_before_it_acted_runs_again() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    std::fs::write(&f.manifest, with_boot_digest(WEB_VM, "web", Some("sha256:web-2"))).unwrap();
    f.engine().refusals.insert("reload", Refusal::index(1, "web"));
    let events = Recorder::new();
    let updated = f.runner.open(&f.manifest, None).unwrap().update(&opts(), &events).await.unwrap();
    assert_eq!(calls(&f, "vagrant reload web --no-provision"), 2, "{:?}", f.vagrant());
    assert!(warned_retrying(&events), "{:?}", events.events());
    assert_eq!(updated.record.vm_boot_digests.get("web").map(String::as_str), Some("sha256:web-2"));
}

/// A lock held past [`super::lock::PATIENCE`] still fails the stop, with Vagrant's own error,
/// after a bounded number of tries.
#[tokio::test(start_paused = true)]
async fn a_destroy_locked_out_past_the_patience_still_fails_with_vagrants_error() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.engine().refusals.insert("destroy", Refusal::index(u32::MAX, "web"));
    let err = run.down(&RunOptions::new(), &Recorder::new()).await.unwrap_err().to_string();
    assert!(
        err.starts_with("step `destroy` failed: Vagrant can't use the requested machine because it is locked!") && !err.contains("[fog]"),
        "{err}"
    );
    let tries = calls(&f, "vagrant destroy --force");
    assert!((2..=32).contains(&tries), "{tries} tries");
}

/// The refusal the app showed (a Mac on vagrant-qemu): its status poll read the run while the
/// attack box's `vagrant up` held `attackbox`, and every read failed with Vagrant's own words.
const ATTACKBOX_LOCKED: &str = "An action 'read_state' was attempted on the machine 'attackbox',\nbut another process is \
                                already executing an action on the machine.\nVagrant locks each machine for access by only \
                                one process at a time. (Vagrant::Errors::MachineActionLockedError) (exited with code 1)";

/// Regression (the app's status poll, on vagrant-qemu): another process's `vagrant up` brought
/// `attackbox` up and provisioned it, no operation of this runner holding the run, and each status
/// read, refused by Vagrant's action lock on it, was tried again for a minute, then failed with
/// Vagrant's refusal (`An action 'read_state' was attempted on the machine 'attackbox', …
/// (Vagrant::Errors::MachineActionLockedError) (exited with code 1)`): the app showed it as the
/// lab's error. A status read refused so is no failure: the run as last read, `attackbox` busy,
/// not live; once the lock is gone, it is read again.
#[tokio::test(start_paused = true)]
async fn a_status_read_refused_for_another_process_acting_on_a_machine_says_it_busy() {
    let f = fixture_from(crate::runner::this_host(), &with_vm_added("attackbox"));
    f.engine().added.push("attackbox".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    // The attack box's start, in another process: its `vagrant up` holds `attackbox`.
    f.engine().acting_on = Some("attackbox");
    let status = run.status().await.unwrap_or_else(|e| {
        assert_eq!(e.to_string(), format!("step `status` failed: {ATTACKBOX_LOCKED}"), "Vagrant's own refusal");
        panic!("a status read another process's lock refused failed: {e}")
    });
    assert_eq!((status.state, status.live), (RunState::Ready, false));
    let state = |name: &str| status.machines.iter().find(|m| m.name == name).map(|m| m.state.clone());
    assert_eq!(state("attackbox"), Some(crate::MachineState::Busy), "{:?}", status.machines);
    assert_eq!(state("web"), Some(crate::MachineState::Running), "as last read");
    f.engine().acting_on = None;
    let after = run.status().await.unwrap();
    assert_eq!((after.state, after.live), (RunState::Ready, true));
    assert!(after.machines.iter().all(|m| m.state != crate::MachineState::Busy), "{:?}", after.machines);
}

/// Regression (the Screen button, on vagrant-qemu): `attackbox`'s screen, asked for while the
/// attack box's `vagrant up` held it, read the run's status for a minute, Vagrant refusing each
/// read, then failed with Vagrant's refusal, the viewer spinning all along. It is refused within
/// a query's patience, saying why. While an operation holds the run, Vagrant isn't asked at all:
/// the machine is as last read, and its hypervisor is asked where its screen is.
#[tokio::test(start_paused = true)]
async fn a_screen_asked_for_while_another_process_acts_on_its_machine_is_refused_saying_so() {
    let f = fixture_from(crate::runner::this_host(), &with_vm_added("attackbox"));
    f.engine().added.push("attackbox".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    f.engine().acting_on = Some("attackbox");
    let asked = tokio::time::Instant::now();
    let err = run.graphical_console("attackbox").await.unwrap_err();
    assert!(asked.elapsed() <= super::super::lock::QUERY_PATIENCE + std::time::Duration::from_secs(2));
    assert_eq!(
        (err.code(), err.to_string()),
        (
            ErrorCode::NoConsole,
            "machine `attackbox` has no graphical console: another process is acting on it (starting or provisioning it): its \
             screen can be shown once that is done"
                .to_owned()
        )
    );
    // Another process's operation holds the run: Vagrant isn't asked.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.folder.home.path().join("runs/web-vm/lock"))
        .unwrap();
    held.lock().unwrap();
    let reads = f.vagrant().iter().filter(|l| l.starts_with("vagrant status")).count();
    let web = run.graphical_console("web").await.unwrap_err();
    assert_eq!(f.vagrant().iter().filter(|l| l.starts_with("vagrant status")).count(), reads);
    assert!(web.to_string().contains("VirtualBox"), "its hypervisor was asked: {web}");
}
