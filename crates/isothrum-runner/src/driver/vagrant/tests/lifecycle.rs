//! A VM run's life through the fake Vagrant: up, check, park, resume, down; the provider and
//! tool checks before anything runs; the leftovers and orphaned VMs a run clears.

use super::*;

#[tokio::test]
async fn a_vm_run_comes_up_is_checked_parked_resumed_and_goes() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.state, RunState::Ready);
    assert_eq!(started.record.provider.as_deref(), Some("virtualbox"));
    assert_eq!(f.state(CONTROLLER), "poweroff", "the controller is halted once the run is built");
    // The published port is ephemeral: a free host port, picked at the first start.
    assert_eq!(started.outputs[0].value.as_deref(), Some("http://127.0.0.1:41000/"));
    assert_eq!(started.message.as_deref(), Some("The page answers at http://localhost:41000/."));

    let report = run.check(&opts(), &Recorder::new()).await.unwrap();
    assert!(report.ok, "{report:?}");
    assert_eq!(f.state(CONTROLLER), "poweroff", "booted for the checks, halted after");
    assert!(
        f.vagrant().contains(&"vagrant up isoloom-controller --no-provision".to_owned()),
        "{:?}",
        f.vagrant()
    );

    run.park(Park::Pause, &RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "saved");
    assert_eq!(run.status().await.unwrap().state, RunState::Parked);
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(
        (f.state("web").as_str(), f.state(CONTROLLER).as_str()),
        ("running", "poweroff"),
        "the controller stays off"
    );
    run.park(Park::Shutdown, &RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "poweroff");
    run.resume(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "running");

    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert_eq!(f.state("web"), "not_created");
    assert_eq!(run.status().await.unwrap().state, RunState::Absent);
}

/// A VM run that declares no access machine has none to attach: the runner adds no VM of its own
/// to the running environment, and nothing is booted.
#[tokio::test]
async fn a_vm_run_that_declares_no_access_machine_has_none_to_attach() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    crate::testing::refuses_a_box_it_has_none_of(&run, &f.fake).await;
}

#[tokio::test]
async fn vagrants_record_lives_in_the_runs_state_folder() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts(), &Recorder::new()).await.unwrap();
    let dotfile = f.runner.store.run_dir(run.name()).unwrap().join("vagrant");
    for call in f.fake.calls().iter().filter(|c| c.program == "vagrant") {
        assert_eq!(call.env("VAGRANT_DOTFILE_PATH").map(PathBuf::from), Some(dotfile.clone()), "{}", call.line());
    }
    assert!(dotfile.join("machines/web/virtualbox/id").is_file());
    assert!(!f.manifest.parent().unwrap().join(".vagrant").exists(), "nothing next to the Vagrantfile");
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    assert!(!dotfile.exists(), "reset once the VMs are gone");
}

#[tokio::test]
async fn the_flag_reaches_the_start_and_never_an_event_and_an_unset_one_is_empty() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let up = f.fake.last("vagrant up --provider").unwrap();
    assert_eq!(up.env("DEMO_SECRET"), Some(SECRET));
    assert_eq!(
        f.fake.last("vagrant halt").unwrap().env("DEMO_SECRET"),
        None,
        "only the steps that run provisioners"
    );
    let text = serde_json::to_string(&events.events()).unwrap();
    assert!(!text.contains(SECRET) && text.contains("seeding ***"), "{text}");
    // ESXi's connection never reaches a VirtualBox run.
    assert!(
        f.fake
            .calls()
            .iter()
            .all(|c| c.env("ESXI_PASSWORD").is_none() && c.env("ESXI_HOSTNAME").is_none())
    );
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    run.up(&RunOptions::new().with_provider("virtualbox"), &Recorder::new()).await.unwrap();
    assert_eq!(f.fake.last("vagrant up --provider").unwrap().env("DEMO_SECRET"), Some(""), "its default");
}

#[tokio::test]
async fn a_provider_that_cant_run_the_machines_is_refused_before_anything_runs() {
    let f = fixture(HostOs::Macos);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let code = |r: crate::Result<crate::Started>| r.map(drop).unwrap_err();
    let none = code(run.up(&RunOptions::new(), &Recorder::new()).await);
    assert_eq!(none.code(), ErrorCode::ProviderRefused);
    assert!(none.to_string().contains("needs a provider"), "{none}");
    // UTM gives a VM no private network: Isoloom's refusal, in its words.
    let utm = code(run.up(&RunOptions::new().with_provider("utm"), &Recorder::new()).await);
    assert!(utm.to_string().contains("vagrant_utm gives a VM no private network"), "{utm}");
    let libvirt = code(run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await);
    assert!(libvirt.to_string().contains("doesn't exist on this host"), "{libvirt}");
    assert!(f.vagrant().is_empty(), "{:?}", f.vagrant());
}

#[tokio::test]
async fn a_vagrant_older_than_2_4_is_refused_before_anything_runs() {
    // Debian's packaged 2.3.8 ignores a box's architecture: it booted a ppc64le Debian on amd64.
    let f = fixture(crate::runner::this_host());
    f.engine().version = Some("2.3.8.dev".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&opts(), &Recorder::new()).await.unwrap_err();
    assert_eq!(err.code(), ErrorCode::ToolTooOld);
    assert_eq!(err.to_string(), "`vagrant` is version 2.3.8; this run needs 2.4.0 or later");
    assert!(f.vagrant().is_empty(), "{:?}", f.vagrant());
}

#[tokio::test]
async fn a_providers_missing_plugin_is_named_with_its_install_command() {
    let f = fixture(HostOs::Linux);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let err = run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await.unwrap_err();
    assert_eq!(
        err.to_string(),
        "provider `libvirt`: needs the Vagrant plugin `vagrant-libvirt`: `vagrant plugin install vagrant-libvirt`"
    );
    f.engine().plugins.push("vagrant-libvirt".into());
    run.up(&RunOptions::new().with_provider("libvirt"), &Recorder::new()).await.unwrap();
    assert_eq!(f.engine().provider.as_deref(), Some("libvirt"));
}

#[tokio::test]
async fn a_start_that_tripped_over_leftovers_clears_them_and_starts_again() {
    // VirtualBox prints VERR_ALREADY_EXISTS in capitals; the manifest's markers are lower case.
    let f = fixture(crate::runner::this_host());
    f.engine()
        .ups
        .push(("VBoxManage: error: Details: code VERR_ALREADY_EXISTS (0x80004005)".into(), true));
    f.engine().vbox.insert("web-vm - web".into(), "poweroff".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let calls = f.fake.lines();
    let at = |text: &str| calls.iter().position(|l| l.contains(text)).unwrap_or_else(|| panic!("{text}: {calls:#?}"));
    assert!(at("vagrant destroy --force") < at("VBoxManage unregistervm uuid-web-vm - web --delete"));
    assert_eq!(calls.iter().filter(|l| l.starts_with("vagrant up --provider")).count(), 2);
    assert!(f.engine().vbox.is_empty(), "the orphan is gone");
}

/// A run on Parallels whose `down` finds its VM still registered: `prlctl` stops and deletes it
/// by its exact name, and leaves the others.
#[tokio::test]
async fn a_down_on_parallels_removes_the_runs_orphaned_vm_by_its_exact_name() {
    let f = fixture(HostOs::Macos);
    f.engine().plugins.push("vagrant-parallels".into());
    f.engine().parallels = Some(["web-vm - web", "web-vm-web", "mine"].map(String::from).into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts().with_provider("parallels"), &Recorder::new()).await.unwrap();
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    let calls = f.fake.lines();
    assert!(calls.contains(&"prlctl stop web-vm - web --kill".to_owned()), "{calls:#?}");
    assert_eq!(f.engine().parallels.clone().unwrap(), ["mine", "web-vm-web"].map(String::from).into());
}

/// Regression: a `prlctl list` that failed skipped the clean-up without a word.
#[tokio::test]
async fn a_hypervisor_listing_that_fails_is_traced_and_removes_nothing() {
    let f = fixture(HostOs::Macos);
    f.engine().plugins.push("vagrant-parallels".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts().with_provider("parallels"), &Recorder::new()).await.unwrap();
    let (down, traced) = crate::testing::traced(run.down(&RunOptions::new(), &Recorder::new())).await;
    down.unwrap();
    assert!(!f.fake.lines().iter().any(|l| l.starts_with("prlctl delete")));
    assert!(
        traced.contains("WARN") && traced.contains("couldn't be read") && traced.contains("the Parallels service isn't running"),
        "{traced}"
    );
}
