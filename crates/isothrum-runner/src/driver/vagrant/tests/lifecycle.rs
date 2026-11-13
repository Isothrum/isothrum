//! A VM run's life through the fake Vagrant: up, check, park, resume, down; the provider and
//! tool checks before anything runs; the leftovers and orphaned VMs a run clears.

use super::*;

#[tokio::test]
async fn a_vm_run_comes_up_is_checked_parked_resumed_and_goes() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let started = run.up(&opts(), &Recorder::new()).await.unwrap();
    assert_eq!(started.record.state, RunState::Ready);
    assert_eq!(started.record.provider, Some(isoloom_manifest::Provider::Virtualbox));
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
    run.up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Virtualbox), &Recorder::new())
        .await
        .unwrap();
    assert_eq!(f.fake.last("vagrant up --provider").unwrap().env("DEMO_SECRET"), Some(""), "its default");
}

#[tokio::test]
async fn a_provider_that_cant_run_the_machines_is_refused_before_anything_runs() {
    // Isoloom refuses a provider the run's machines can't use in the manifest: Parallels here.
    let mut m: serde_json::Value = serde_json::from_str(WEB_VM).unwrap();
    let providers = m["identity"]["vagrant"]["providers"].as_array_mut().unwrap();
    let parallels = providers.iter_mut().find(|p| p["id"] == "parallels").unwrap();
    parallels["refusal"] = "`web` needs something Parallels doesn't give".into();
    let f = fixture_from(HostOs::Macos, &m.to_string());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let code = |r: crate::Result<crate::Started>| r.map(drop).unwrap_err();
    let none = code(run.up(&RunOptions::new(), &Recorder::new()).await);
    assert_eq!(none.code(), ErrorCode::ProviderRefused);
    // Regression: it said ``provider ``: a Vagrant run needs a provider…``, an empty name in
    // backticks, and a list of its own; the run's providers are the manifest's.
    assert_eq!(
        none.to_string(),
        "a Vagrant run needs a provider: pass one of this run's (`virtualbox`, `vmware_desktop`, `hyperv`, `parallels`, `libvirt`, `qemu`, `vmware_esxi`)"
    );
    // Isoloom's refusal, in its words.
    let refused = code(
        run.up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Parallels), &Recorder::new())
            .await,
    );
    assert!(refused.to_string().contains("needs something Parallels doesn't give"), "{refused}");
    // UTM isn't a VM run's provider: Isoloom offers it to docker-vm only.
    let utm = code(
        run.up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Utm), &Recorder::new())
            .await,
    );
    assert!(utm.to_string().contains("isn't one of this run's providers"), "{utm}");
    let libvirt = code(
        run.up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Libvirt), &Recorder::new())
            .await,
    );
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
    let err = run
        .up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Libvirt), &Recorder::new())
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "provider `libvirt`: needs the Vagrant plugin `vagrant-libvirt`: `vagrant plugin install vagrant-libvirt`"
    );
    f.engine().plugins.push("vagrant-libvirt".into());
    run.up(&RunOptions::new().with_provider(isoloom_manifest::Provider::Libvirt), &Recorder::new())
        .await
        .unwrap();
    assert_eq!(f.engine().provider.as_deref(), Some("libvirt"));
}

/// A fixture whose first `vagrant up` trips over the VM an earlier run left registered in
/// VirtualBox (`web-vm-<key> - web`), beside another copy's of the same environment (its own
/// key), which is never this run's to remove. VirtualBox prints `VERR_ALREADY_EXISTS` in
/// capitals; the manifest's markers are lower case.
fn tripped_over_an_orphan() -> Fixture {
    let f = fixture(crate::runner::this_host());
    f.engine()
        .ups
        .push(("VBoxManage: error: Details: code VERR_ALREADY_EXISTS (0x80004005)".into(), true));
    let mine = f.vm("web");
    f.engine().vbox.insert(mine, "poweroff".into());
    f.engine().vbox.insert(ANOTHER_COPY.into(), "running".into());
    f
}

/// Another copy's VM of the same environment: another key.
const ANOTHER_COPY: &str = "web-vm-zz0000 - web";

#[tokio::test]
async fn a_start_that_tripped_over_leftovers_clears_them_and_starts_again() {
    let f = tripped_over_an_orphan();
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.unwrap();
    let calls = f.fake.lines();
    let at = |text: &str| calls.iter().position(|l| l.contains(text)).unwrap_or_else(|| panic!("{text}: {calls:#?}"));
    assert!(at("vagrant destroy --force") < at(&format!("VBoxManage unregistervm uuid-{} --delete", f.vm("web"))));
    assert_eq!(calls.iter().filter(|l| l.starts_with("vagrant up --provider")).count(), 2);
    // Regression (#109): the sweep matched the manifest's names, which held no run's identity, and
    // deleted another run's VM of the same environment beside its own orphan.
    assert_eq!(
        f.engine().vbox.keys().collect::<Vec<_>>(),
        [ANOTHER_COPY],
        "the orphan is gone, the other copy's VM kept"
    );
    assert!(!calls.iter().any(|l| l.contains(&format!("uuid-{ANOTHER_COPY}"))), "{calls:#?}");
}

/// A run on Parallels whose `down` finds its VM still registered: `prlctl` stops and deletes it
/// by its exact name, and leaves the others.
#[tokio::test]
async fn a_down_on_parallels_removes_the_runs_orphaned_vm_by_its_exact_name() {
    let f = fixture(HostOs::Macos);
    f.engine().parallels = Some([f.vm("web"), "web-vm - web".into(), ANOTHER_COPY.into(), "mine".into()].into());
    let run = up_on_parallels(&f).await;
    run.down(&RunOptions::new(), &Recorder::new()).await.unwrap();
    let calls = f.fake.lines();
    assert!(calls.contains(&format!("prlctl stop {} --kill", f.vm("web"))), "{calls:#?}");
    assert_eq!(
        f.engine().parallels.clone().unwrap(),
        ["mine", "web-vm - web", ANOTHER_COPY].map(String::from).into()
    );
}

/// Regression: a `prlctl list` that failed skipped the clean-up without a word; then it was only
/// traced, which an embedder with no subscriber never sees. It is a `cleanup_failed` warning, as a
/// removal that fails is.
#[tokio::test]
async fn a_hypervisor_listing_that_fails_is_a_cleanup_warning_and_removes_nothing() {
    let f = fixture(HostOs::Macos);
    let run = up_on_parallels(&f).await;
    let (said, traced) = crate::testing::traced(down_warnings(&run)).await;
    assert!(!f.fake.lines().iter().any(|l| l.starts_with("prlctl delete")));
    assert_eq!(
        said,
        [
            "cleanup_failed: what earlier runs left couldn't be listed (`prlctl list -a --no-header -o name`), so none of it is removed: \
          Failed to get the list of VMs: the Parallels service isn't running (exited with code 1)"
        ]
    );
    assert!(
        traced.contains("WARN") && traced.contains("couldn't be read") && traced.contains("the Parallels service isn't running"),
        "{traced}"
    );
}

/// A run of `f` started on Parallels, its plugin installed.
async fn up_on_parallels(f: &Fixture) -> crate::Running<'_> {
    f.engine().plugins.push("vagrant-parallels".into());
    let run = f.runner.open(&f.manifest, None).unwrap();
    run.up(&opts().with_provider(isoloom_manifest::Provider::Parallels), &Recorder::new())
        .await
        .unwrap();
    run
}

/// The warnings of a `down` of `run` that succeeds.
async fn down_warnings(run: &crate::Running<'_>) -> Vec<String> {
    let events = Recorder::new();
    run.down(&RunOptions::new(), &events).await.unwrap();
    warnings(&events)
}

/// The warnings of `events`, as `code: message`.
fn warnings(events: &Recorder) -> Vec<String> {
    events
        .events()
        .into_iter()
        .filter_map(|e| match e.kind {
            crate::EventKind::Warning { code, message } => Some(format!("{}: {message}", crate::encoding::wire_name(&code))),
            _ => None,
        })
        .collect()
}

/// Regression: an orphaned VM was announced removed before the hypervisor was asked, and a
/// deletion it refused was only traced at debug: the events said it was gone, and the next import
/// still met it.
#[tokio::test]
async fn an_orphan_the_hypervisor_wont_delete_is_warned_about_not_said_removed() {
    let f = tripped_over_an_orphan();
    f.engine().fails.insert(Fails::Delete);
    let run = f.runner.open(&f.manifest, None).unwrap();
    let events = Recorder::new();
    run.up(&opts(), &events).await.ok();
    let said = warnings(&events);
    assert!(!said.iter().any(|w| w.starts_with("orphan_removed")), "{said:#?}");
    assert!(
        said.iter()
            .any(|w| w.starts_with(&format!("cleanup_failed: a VM an earlier run left ({}) couldn't be removed: ", f.vm("web"))) && w.contains(VBOX_LOCKED)),
        "{said:#?}"
    );
}

#[tokio::test]
async fn an_orphan_parallels_wont_delete_is_warned_about_and_one_it_deletes_said_removed() {
    for undeletable in [false, true] {
        let f = fixture(HostOs::Macos);
        f.engine().parallels = Some([f.vm("web")].into());
        if undeletable {
            f.engine().fails.insert(Fails::Delete);
        }
        let said = down_warnings(&up_on_parallels(&f).await).await;
        let removed = said
            .iter()
            .any(|w| *w == format!("orphan_removed: removed a VM an earlier run left ({})", f.vm("web")));
        let refused = said
            .iter()
            .any(|w| w.starts_with(&format!("cleanup_failed: a VM an earlier run left ({}) couldn't be removed: ", f.vm("web"))));
        assert_eq!((removed, refused), (!undeletable, undeletable), "{said:#?}");
    }
}

/// A machine folder that can't be listed is a failed clean-up, said as one (it was a tolerated
/// step's warning, though no step of the manifest's had failed); a leftover folder removed is said
/// once it is gone.
#[tokio::test]
async fn virtualbox_leftover_folders_are_said_removed_once_gone_and_an_unreadable_folder_is_a_failed_clean_up() {
    let machines = tempfile::tempdir().unwrap();
    let not_a_folder = machines.path().join("a-file");
    std::fs::write(&not_a_folder, "").unwrap();
    let theirs = machines.path().join(ANOTHER_COPY);
    std::fs::create_dir(&theirs).unwrap();
    for listable in [true, false] {
        let f = fixture(crate::runner::this_host());
        let leftover = machines.path().join(f.vm("web"));
        std::fs::create_dir(&leftover).unwrap();
        let (folder, expected) = if listable {
            (
                machines.path().to_path_buf(),
                format!("orphan_removed: removed a VM folder an earlier run left ({})", leftover.display()),
            )
        } else {
            (
                not_a_folder.clone(),
                format!(
                    "cleanup_failed: the VM folders an earlier run left in {} couldn't be removed: ",
                    not_a_folder.display()
                ),
            )
        };
        f.engine().machine_folder = Some(folder);
        let run = f.runner.open(&f.manifest, None).unwrap();
        run.up(&opts(), &Recorder::new()).await.unwrap();
        let events = Recorder::new();
        run.down(&RunOptions::new(), &events).await.unwrap();
        let said = warnings(&events);
        assert!(said.iter().any(|w| w.starts_with(&expected)), "{expected}\n{said:#?}");
        assert!(!said.iter().any(|w| w.starts_with("step_tolerated")), "{said:#?}");
        assert_eq!(!leftover.exists(), listable);
    }
    assert!(theirs.is_dir(), "another copy's VM folder is never this run's leftover");
}

/// Every Vagrant command of a run gets the run's key (`ISOTHRUM_RUN_KEY`), the same at each, and
/// the variables the embedder gives the operation (a project's own name for the key, say); a
/// driver's own variable (`VAGRANT_DOTFILE_PATH`) wins over an embedder's of the same name.
#[tokio::test]
async fn every_vagrant_command_gets_the_runs_key_and_the_embedders_variables() {
    let f = fixture(crate::runner::this_host());
    let run = f.runner.open(&f.manifest, None).unwrap();
    let key = f.runner.copy_key(run.name());
    let given = opts().with_env("PROJECT_KEY", key.clone()).with_env("VAGRANT_DOTFILE_PATH", "/elsewhere");
    run.up(&given, &Recorder::new()).await.unwrap();
    let acting = |c: &crate::process::ProcessSpec| ["vagrant up", "vagrant status", "vagrant halt"].iter().any(|v| c.line().starts_with(v));
    let vagrant: Vec<_> = f.fake.calls().into_iter().filter(|c| acting(c)).collect();
    assert!(vagrant.iter().any(|c| c.line().starts_with("vagrant up")), "{vagrant:#?}");
    for call in &vagrant {
        assert_eq!(call.env(crate::RUN_KEY_VAR), Some(key.as_str()), "{}", call.line());
        assert_eq!(call.env("PROJECT_KEY"), Some(key.as_str()), "{}", call.line());
        assert_ne!(call.env("VAGRANT_DOTFILE_PATH"), Some("/elsewhere"), "{}", call.line());
    }
}
