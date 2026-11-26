//! What setup plans for each hypervisor: one KVM holds, Hyper-V's restart, VMware's utility and plugin, and those wanted but not offered.

use super::*;

#[test]
fn a_hypervisor_joined_since_logging_in_asks_for_a_new_login_and_one_that_cant_run_is_blocked() {
    let mut m = bare(HostOs::Linux, vec![status(Provider::Libvirt, true, None)]);
    m.groups_pending = vec!["libvirt".into()];
    assert_eq!(options(&m, false).hypervisors[0].get, [Action::Relogin]);
    m.groups_pending.clear();
    m.providers = vec![status(Provider::Libvirt, true, Some(Unavailable::NoKvm))];
    let o = options(&m, false);
    assert!(cant_run(&m.providers[0]));
    assert_eq!(o.hypervisors[0].get, [Action::Blocked { reason: Unavailable::NoKvm }]);
    assert!(left_alone(&o.hypervisors[0].get[0]).unwrap().fails());
    let p = plan(&m, &o, &wanted(&[SetupPath::Vm], &[]));
    assert_eq!(p.passed_over, [(Provider::Libvirt, PassedOver::CantRun { reason: Unavailable::NoKvm })]);
}

#[test]
fn virtualbox_while_kvm_holds_the_cpus_virtualization_gives_way_unless_wanted_then_kvm_shares_it() {
    let m = bare(
        HostOs::Linux,
        vec![
            status(Provider::Virtualbox, false, Some(Unavailable::KvmHoldsVirtualization)),
            status(Provider::Qemu, false, None),
        ],
    );
    let o = options(&m, false);
    assert_eq!(
        o.hypervisors[0].get,
        [
            Action::Install {
                dependency: Dependency::Virtualbox,
                interactions: vec![PKEXEC],
            },
            Action::ShareVirtualization { interactions: vec![PKEXEC] }
        ]
    );
    assert_eq!(o.hypervisor, Some(Provider::Qemu), "one that just installs comes first");
    let default = plan(&m, &o, &wanted(&[SetupPath::Vm], &[]));
    assert_eq!(default.hypervisor, Some(Provider::Qemu));
    assert_eq!(steps(&default), named(&[("qemu", "install")]));
    let named_vbox = plan(&m, &o, &wanted(&[SetupPath::Vm], &[Provider::Virtualbox]));
    assert_eq!(named_vbox.hypervisor, Some(Provider::Virtualbox));
    assert_eq!(steps(&named_vbox), named(&[("virtualbox", "install"), ("virtualbox", "share")]));
    let share = Action::ShareVirtualization { interactions: vec![PKEXEC] };
    assert!(left_alone(&share).is_none() && !share.ends_pass(), "a setup runs it");
}

#[test]
fn virtualbox_installed_while_kvm_holds_it_is_the_one_kvm_shares_with_or_a_restart_away() {
    let mut m = bare(
        HostOs::Linux,
        vec![
            status(Provider::Virtualbox, true, Some(Unavailable::KvmHoldsVirtualization)),
            status(Provider::Qemu, false, None),
        ],
    );
    let p = plan_of(&m, &wanted(&[SetupPath::Vm], &[]));
    assert_eq!(p.hypervisor, Some(Provider::Virtualbox), "installed: the one its user set up");
    assert_eq!(steps(&p), named(&[("virtualbox", "share")]));
    assert_eq!(
        serde_json::to_value(&p.steps[0].action).unwrap(),
        serde_json::json!({ "kind": "shareVirtualization", "interactions": [{ "kind": "adminPassword", "via": "pkexec" }] })
    );

    // KVM was in use: the option applies from the next restart, which ends the pass.
    m.providers[0] = status(Provider::Virtualbox, true, Some(Unavailable::KvmSharesAfterRestart));
    let restart = plan_of(&m, &wanted(&[SetupPath::Vm], &[]));
    assert_eq!(restart.hypervisor, Some(Provider::Virtualbox));
    assert_eq!(steps(&restart), named(&[("virtualbox", "blocked")]));
    assert!(restart.steps[0].action.ends_pass());
}

#[test]
fn wanted_hypervisors_not_offered_or_not_needed_are_passed_over() {
    let m = bare(HostOs::Linux, vec![status(Provider::Libvirt, false, None), status(Provider::Qemu, false, None)]);
    let p = plan_of(&m, &wanted(&[SetupPath::Vm], &[Provider::Utm, Provider::Qemu, Provider::Libvirt]));
    assert_eq!(p.hypervisor, Some(Provider::Qemu));
    assert_eq!(
        p.passed_over,
        [
            (Provider::Utm, PassedOver::NotOffered),
            (Provider::Libvirt, PassedOver::NotNeeded { chosen: Provider::Qemu }),
        ]
    );
}

#[test]
fn hyper_v_turned_on_waits_for_a_restart_and_vmware_links_out_with_what_vagrant_needs() {
    let mut vmware = status(Provider::VmwareDesktop, false, None);
    vmware.plugin = Some("vagrant-vmware-desktop".into());
    vmware.utility = Some(crate::providers::UtilityState::Missing);
    vmware.needs = needs(false, false, vmware.utility);
    let m = bare(HostOs::Windows, vec![status(Provider::Hyperv, false, None), vmware]);
    let o = options(&m, true);
    assert_eq!(o.hypervisors[0].get[0].kind(), "restart");
    assert_eq!(
        o.hypervisors[1].vagrant.iter().map(Action::kind).collect::<Vec<_>>(),
        ["install"],
        "Vagrant only: VMware's utility and plugin wait for VMware"
    );
    let p = plan(&m, &o, &wanted(&[SetupPath::Vm], &[Provider::VmwareDesktop]));
    assert_eq!(
        p.passed_over,
        [(
            Provider::VmwareDesktop,
            PassedOver::LinksOut {
                url: Some(crate::install::VMWARE_WORKSTATION_PAGE.url.to_owned())
            }
        )]
    );
}

/// Regression: VMware's plugin installed without its utility breaks Vagrant for every provider
/// (studio's QEMU run). It is planned only after the utility's install, in the same plan, or not at
/// all where the utility is its user's to install.
#[test]
fn vmwares_plugin_is_planned_only_after_its_utilitys_install() {
    let vmware = |installed_by_setup: bool| {
        let mut vmware = status(Provider::VmwareDesktop, true, None);
        vmware.plugin = Some("vagrant-vmware-desktop".into());
        vmware.utility = Some(crate::providers::UtilityState::Missing);
        vmware.needs = needs(true, false, vmware.utility);
        let mut m = bare(HostOs::Linux, vec![vmware]);
        m.packages_installed = installed_by_setup;
        m
    };
    let m = vmware(true);
    let o = options(&m, false);
    assert_eq!(
        o.hypervisors[0].vagrant,
        [
            Action::Install {
                dependency: Dependency::VmwareUtility,
                interactions: vec![PKEXEC],
            },
            Action::Plugin {
                plugin: "vagrant-vmware-desktop".into()
            }
        ]
    );
    let by_hand = options(&vmware(false), false);
    assert_eq!(
        by_hand.hypervisors[0].vagrant.iter().map(Action::kind).collect::<Vec<_>>(),
        ["manual"],
        "the utility is its user's to install: no plugin before it"
    );
}
