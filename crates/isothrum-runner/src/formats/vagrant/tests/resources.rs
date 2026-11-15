//! The machines a run made: found by the dotfile's ids, removed by id, and re-applied.

use super::*;

#[tokio::test]
async fn found_is_the_dotfiles_machine_ids_by_machine() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    f.made("old", "virtualbox", "uuid-old");
    let fake = FakeProcesses::script().build();
    let found = Vagrant.found(&cx(&f, &fake, LIBVIRT)).await.expect("found");
    assert_eq!(
        found,
        [
            Found {
                key: "vm:old@virtualbox".into(),
                kind: ResourceKind::Vm,
                native_id: "uuid-old".into(),
                lifetime: Lifetime::Machines,
            },
            Found {
                key: "vm:web".into(),
                kind: ResourceKind::Vm,
                native_id: "uuid-web".into(),
                lifetime: Lifetime::Machines,
            },
        ]
    );
    assert_eq!(made(&fake), Vec::<String>::new(), "read from the dotfile, no call");
}

#[tokio::test]
async fn destroy_by_id_destroys_a_recorded_machine_through_vagrant() {
    let f = Fixture::new();
    f.made("web", "libvirt", "uuid-web");
    let fake = FakeProcesses::script().expect_times("vagrant destroy -f web", 1, Reply::ok("")).build();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "vm:web", ResourceKind::Vm, "uuid-web")
        .await
        .expect("destroyed");
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

#[tokio::test]
async fn destroy_by_id_writes_a_lost_record_back_for_vagrant_to_destroy_by() {
    let f = Fixture::new();
    let fake = FakeProcesses::script().expect_times("vagrant destroy -f web", 1, Reply::ok("")).build();
    let dot = f.dot();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "vm:web", ResourceKind::Vm, "uuid-web")
        .await
        .expect("destroyed");
    let call = fake.last("destroy").unwrap();
    assert_eq!(call.env(dotfile::VAR), Some(dot.path.display().to_string().as_str()));
    assert_eq!(dot.recorded().unwrap(), Vec::<Record>::new(), "the written-back record goes with its VM");
}

#[tokio::test]
async fn destroy_by_id_of_a_machine_no_longer_defined_goes_by_id_at_virtualbox_and_gone_is_fine() {
    let f = Fixture::new();
    f.made("old", "virtualbox", "uuid-old");
    let not_found = Reply {
        code: 1,
        ..Reply::ok(&fixture("vagrant/2.4.9-machine-not-found.txt"))
    };
    let fake = FakeProcesses::script()
        .expect_times("vagrant destroy -f old", 1, not_found)
        .expect("VBoxManage controlvm uuid-old poweroff", Reply::fail(1, "not running"))
        .expect_times(
            "VBoxManage unregistervm uuid-old --delete",
            1,
            Reply::fail(1, &fixture("vboxmanage/7.0-showvminfo-not-found.txt")),
        )
        .build();
    Vagrant
        .destroy_by_id(&cx(&f, &fake, VBOX), "vm:old", ResourceKind::Vm, "uuid-old")
        .await
        .expect("already gone is fine");
    assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new());
}

/// The fake for a machine `old` whose `define` left the Vagrantfile, removed by its id `uuid-old`
/// through `tool`'s calls `calls` (each answered `reply`).
fn removed_by(calls: &[&str], reply: &Reply) -> FakeProcesses {
    let not_found = Reply {
        code: 1,
        ..Reply::ok(&fixture("vagrant/2.4.9-machine-not-found.txt"))
    };
    let mut script = FakeProcesses::script().expect("vagrant destroy -f old", not_found);
    for call in calls {
        script = script.expect(call, reply.clone());
    }
    script.build()
}

/// Regression: a VM whose `define` left the Vagrantfile was removable by its id on VirtualBox
/// only; on libvirt, Parallels and UTM the removal was refused and the VM kept running.
#[tokio::test]
async fn destroy_by_id_of_a_machine_no_longer_defined_goes_through_its_hypervisor() {
    for (provider, calls) in [
        (
            Provider::Libvirt,
            &[
                "virsh -c qemu:///system destroy uuid-old",
                "virsh -c qemu:///system undefine uuid-old --remove-all-storage --nvram",
            ][..],
        ),
        (Provider::Parallels, &["prlctl stop uuid-old --kill", "prlctl delete uuid-old"][..]),
        (Provider::Utm, &["utmctl stop --kill uuid-old", "utmctl delete uuid-old"][..]),
    ] {
        let f = Fixture::new();
        f.made("old", provider.id(), "uuid-old");
        let fake = removed_by(calls, &Reply::ok(""));
        Vagrant
            .destroy_by_id(&cx(&f, &fake, Some(provider)), "vm:old", ResourceKind::Vm, "uuid-old")
            .await
            .unwrap_or_else(|e| panic!("{provider:?}: {e}"));
        assert_eq!(f.dot().recorded().unwrap(), Vec::<Record>::new(), "{provider:?}: its record goes");
    }
}

/// A VM its hypervisor no longer knows is gone already; any other failure fails the removal and
/// keeps the record.
#[tokio::test]
async fn a_vm_its_hypervisor_doesnt_know_is_gone_and_another_failure_keeps_its_record() {
    let f = Fixture::new();
    f.made("old", "libvirt", "uuid-old");
    let calls = ["virsh -c qemu:///system destroy uuid-old", "virsh -c qemu:///system undefine uuid-old"];
    let gone = removed_by(&calls, &Reply::fail(1, "error: failed to get domain 'uuid-old'"));
    Vagrant
        .destroy_by_id(&cx(&f, &gone, LIBVIRT), "vm:old", ResourceKind::Vm, "uuid-old")
        .await
        .expect("gone already");

    let kept = Fixture::new();
    kept.made("old", "libvirt", "uuid-old");
    let refused = removed_by(&calls, &Reply::fail(1, "error: Failed to connect socket: Permission denied"));
    let e = Vagrant
        .destroy_by_id(&cx(&kept, &refused, LIBVIRT), "vm:old", ResourceKind::Vm, "uuid-old")
        .await
        .expect_err("refused");
    assert_eq!(e.code(), crate::ErrorCode::ToolFailed, "{e}");
    assert_eq!(kept.dot().recorded().unwrap().len(), 1, "the record stays for a later try");
}

#[tokio::test]
async fn destroy_by_id_refuses_what_it_has_no_way_to_remove_naming_it() {
    let f = Fixture::new();
    f.made("old", "vmware_desktop", "/vms/old.vmx");
    let fake = removed_by(&[], &Reply::ok(""));
    let e = Vagrant
        .destroy_by_id(&cx(&f, &fake, Some(Provider::VmwareDesktop)), "vm:old", ResourceKind::Vm, "/vms/old.vmx")
        .await
        .expect_err("no VMware adapter");
    assert!(
        matches!(&e, Error::Unsupported { what } if what.contains("vmware_desktop") && what.contains("/vms/old.vmx")),
        "{e}"
    );
    let container = Vagrant
        .destroy_by_id(&cx(&f, &fake, LIBVIRT), "container:x", ResourceKind::Container, "abc")
        .await
        .expect_err("no container in a Vagrant run");
    assert!(matches!(container, Error::Unsupported { .. }));
}

#[tokio::test]
async fn reapply_adds_removes_and_provisions_machine_by_machine() {
    let f = Fixture::new();
    f.made("gone", "virtualbox", "uuid-gone");
    let fake = FakeProcesses::script()
        .expect_times("vagrant up --provider virtualbox attack", 1, Reply::ok(""))
        .expect(
            "vagrant status --machine-readable",
            Reply::ok("1,web,state,running\n1,old,state,running\n1,attack,state,running\n"),
        )
        .expect_times("vagrant destroy -f old", 1, Reply::ok(""))
        .expect("VBoxManage controlvm uuid-gone poweroff", Reply::ok(""))
        .expect_times("VBoxManage unregistervm uuid-gone --delete", 1, Reply::ok(""))
        .expect_times("vagrant provision web", 1, Reply::ok(""))
        .build();
    let change = Change {
        added: vec!["attack".into()],
        removed: vec!["old".into(), "gone".into()],
        changed: vec!["web".into()],
    };
    Vagrant.reapply(&cx(&f, &fake, VBOX), &change).await.expect("reapply");
    let calls = made(&fake);
    let at = |l: &str| calls.iter().position(|c| c == l).unwrap_or_else(|| panic!("{l} in {calls:?}"));
    assert!(at("vagrant up --provider virtualbox attack") < at("vagrant destroy -f old"));
    assert!(at("vagrant destroy -f old") < at("vagrant provision web"));
    assert_eq!(
        f.dot().recorded().unwrap(),
        Vec::<Record>::new(),
        "the machine the Vagrantfile dropped went by its id"
    );
}

/// Regression: a VirtualBox machine folder that couldn't be listed removed nothing and said
/// nothing, so an earlier run's VM folders stayed behind unnoticed.
#[tokio::test]
async fn a_machine_folder_that_cant_be_listed_is_a_cleanup_warning_never_taken_for_none_left() {
    let f = Fixture::new();
    let not_a_folder = f.state.path().join("machines-file");
    std::fs::write(&not_a_folder, "").expect("a file where the folder would be");
    let fake = FakeProcesses::script()
        .expect("VBoxManage list vms", Reply::ok(""))
        .expect(
            "VBoxManage list systemproperties",
            Reply::ok(&format!("Default machine folder:          {}\n", not_a_folder.display())),
        )
        .build();
    Vagrant.virtualbox_leftovers(&cx(&f, &fake, VBOX)).await;
    assert_eq!(f.warnings(), [WarningCode::CleanupFailed]);

    let missing = FakeProcesses::script()
        .expect("VBoxManage list vms", Reply::ok(""))
        .expect(
            "VBoxManage list systemproperties",
            Reply::ok("Default machine folder:          /nowhere/at/all\n"),
        )
        .build();
    Vagrant.virtualbox_leftovers(&cx(&f, &missing, VBOX)).await;
    assert_eq!(f.warnings(), [WarningCode::CleanupFailed], "no folder holds no VM: nothing more is said");
}
