//! Failed starts: what leads the error, a record that can't be read or is on another provider, a
//! VirtualBox VM whose state can't be read or was cut off.

use super::*;

#[tokio::test]
async fn a_genuine_failure_is_never_retried_and_leads_with_ansibles_verdict() {
    let f = fixture(crate::runner::this_host());
    let play = "    isoloom-controller: fatal: [web]: UNREACHABLE! => {\"msg\": \"timed out\"}\nThe SSH command responded with a non-zero exit status.";
    f.engine().ups.push((play.into(), true));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.to_string(), "step `up` failed: fatal: [web]: UNREACHABLE! => {\"msg\": \"timed out\"}");
    assert_eq!(f.fake.lines().iter().filter(|l| l.starts_with("vagrant up --provider")).count(), 1);
}

#[tokio::test]
async fn a_vagrant_crash_leads_with_its_error_not_its_backtrace() {
    // Seen on libvirt: Vagrant prints the error, then dozens of Ruby frames.
    let f = fixture(crate::runner::this_host());
    let error = "/gems/fog-core/wait_for.rb:9:in `block in wait_for': The specified wait_for timeout (2 seconds) was exceeded (Fog::Errors::TimeoutError)";
    let frames = (0..30).map(|n| format!("\tfrom /gems/vagrant/action/builder.rb:{n}:in `call'"));
    let crash = std::iter::once(error.to_owned()).chain(frames).collect::<Vec<_>>().join("\n");
    f.engine().ups.push((crash, true));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err().to_string();
    assert!(
        err.contains("wait_for timeout (2 seconds) was exceeded") && !err.contains("builder.rb"),
        "{err}"
    );
}

/// Regression: on libvirt, every Vagrant call prints fog's warnings on standard error, which led
/// a failed start's error before Vagrant's own.
#[tokio::test]
async fn a_failed_start_leads_with_vagrants_error_not_fogs_warnings() {
    let f = fixture(crate::runner::this_host());
    let said = format!("{FOG_WARNING}\n{FOG_WARNING}\nThe SSH command responded with a non-zero exit status.");
    f.engine().ups.push((said, true));
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "step `up` failed: The SSH command responded with a non-zero exit status. (exited with code 1)"
    );
}

#[tokio::test]
async fn a_record_on_another_provider_is_cleared_before_the_start() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let dotfile = f.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    record(&dotfile, "web", "vmware_desktop");
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let calls = f.vagrant();
    assert_eq!(calls.first().map(String::as_str), Some("vagrant destroy --force"), "{calls:?}");
    assert!(!dotfile.join("machines/web/vmware_desktop").exists());
    // The clean-up is shown as it goes; a read of the driver's own (the plugins) is not.
    let logged: Vec<String> = events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Log { line, .. } => Some(line),
            _ => None,
        })
        .collect();
    assert!(logged.iter().any(|l| l.contains("Destroying VM")), "{logged:?}");
    assert!(!logged.iter().any(|l| l.contains("plugin-name")), "{logged:?}");
    // On its own provider, nothing is cleared.
    let again = fixture(crate::runner::this_host());
    let same = again.runner.open(&again.manifest, None).unwrap();
    record(&again.runner.store.run_dir(same.name()).unwrap().join("vagrant"), "web", "virtualbox");
    same.up(&opts().with_occupied(crate::Occupied::Replace), &Recorder::new()).await.unwrap();
    assert!(!again.vagrant().iter().take(1).any(|l| l.contains("destroy")), "{:?}", again.vagrant());
}

/// Regression: the destroy of the run left on another provider, which the runner does on its own,
/// was said as a tolerated step's failure when it failed.
#[tokio::test]
async fn a_failed_destroy_of_the_run_on_another_provider_is_a_failed_clean_up() {
    let f = fixture(crate::runner::this_host());
    f.engine().fails.insert(Fails::Destroy);
    let run = f.runner.open(&f.manifest, None).unwrap();
    record(&f.runner.store.run_dir(run.name()).unwrap().join("vagrant"), "web", "vmware_desktop");
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let codes: Vec<crate::WarningCode> = events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Warning { code, .. } => Some(code),
            _ => None,
        })
        .collect();
    assert!(codes.contains(&crate::WarningCode::CleanupFailed), "{codes:?}");
    assert!(!codes.contains(&crate::WarningCode::StepTolerated), "{codes:?}");
}

/// Regression: a Vagrant record that couldn't be read read as no record, so a run left on another
/// provider was never cleared first. A file among the machines' folders is no machine.
#[tokio::test]
async fn a_vagrant_record_that_cant_be_read_fails_the_start() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let dotfile = f.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    std::fs::create_dir_all(&dotfile).unwrap();
    std::fs::write(dotfile.join("machines"), "not a folder").unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::Io, "{err}");
    assert!(err.to_string().contains("machines"), "{err}");
    assert!(f.vagrant().is_empty(), "nothing ran: {:?}", f.vagrant());

    let again = fixture(crate::runner::this_host());
    let same = again.runner.open(&again.manifest, None).unwrap();
    let kept = again.runner.store.run_dir(same.name()).unwrap().join("vagrant");
    record(&kept, "web", "virtualbox");
    std::fs::write(kept.join("machines").join(".DS_Store"), "").unwrap();
    same.up(&opts().with_occupied(crate::Occupied::Replace), &Recorder::new()).await.unwrap();
}

/// A VM whose state VirtualBox can't tell is skipped with why in a trace, not in silence.
#[tokio::test]
async fn a_virtualbox_vm_whose_state_cant_be_read_is_skipped_and_traced() {
    let f = fixture(crate::runner::this_host());
    f.engine().vbox.insert("web-vm - web".into(), "aborted-saved".into());
    f.engine().vbox_wedged = true;
    let run = f.runner.open(&f.manifest, None).unwrap();
    let (started, traced) = crate::testing::traced(run.up(&opts(), &Recorder::new())).await;
    started.unwrap();
    assert!(
        !f.fake.lines().iter().any(|l| l.starts_with("VBoxManage discardstate")),
        "nothing read, nothing discarded"
    );
    assert!(traced.contains("web-vm - web") && traced.contains("NS_ERROR_FAILURE"), "{traced}");
}

#[tokio::test]
async fn a_virtualbox_vm_whose_saved_state_was_cut_off_boots_fresh() {
    let f = fixture(crate::runner::this_host());
    f.engine().vbox.insert("web-vm - web".into(), "aborted-saved".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let calls = f.fake.lines();
    let discard = calls.iter().position(|l| l == "VBoxManage discardstate web-vm - web").expect("discarded");
    assert!(discard < calls.iter().position(|l| l.starts_with("vagrant up --provider")).unwrap());
}
