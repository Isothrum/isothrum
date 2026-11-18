use super::*;
use crate::install::{Interaction, Prompt};
use crate::providers::needs;

const PKEXEC: Interaction = Interaction::AdminPassword { via: Prompt::Pkexec };

/// `provider`'s status: installed or not (`installed`), with Vagrant and its plugin in, so that
/// nothing but the hypervisor is missing; `reason` when it can't run although installed.
fn status(provider: Provider, installed: bool, reason: Option<Unavailable>) -> ProviderStatus {
    let reason = reason.or((!installed).then_some(Unavailable::NotInstalled { program: None }));
    ProviderStatus {
        provider,
        remote: false,
        available: reason.is_none(),
        hypervisor: Some(installed),
        plugin: None,
        plugin_installed: true,
        utility: None,
        needs: Vec::new(),
        reason,
        account_lacks: None,
    }
}

/// A computer running `os` with its package manager and nothing else; its hypervisors `providers`.
fn bare(os: HostOs, providers: Vec<ProviderStatus>) -> Machine {
    // Nothing under it is read but `/etc/group`, missing: `/dev/kvm`'s group is `kvm`.
    let mut host = crate::testing::linux_host(std::path::Path::new("/nonexistent/isothrum-setup"));
    host.os = os;
    Machine {
        host,
        packages_installed: true,
        docker: DockerFound::default(),
        wsl: (os == HostOs::Windows).then_some(WslState::Missing),
        providers,
        groups_pending: Vec::new(),
        elevation: crate::install::Elevation::Prompt,
        user: Some("ada".to_owned()),
    }
}

fn wanted(paths: &[SetupPath], hypervisors: &[Provider]) -> Wanted {
    Wanted {
        paths: paths.to_vec(),
        engine: None,
        hypervisors: hypervisors.to_vec(),
    }
}

fn plan_of(m: &Machine, w: &Wanted) -> Plan {
    plan(m, &options(m, false), w)
}

fn steps(p: &Plan) -> Vec<(String, &'static str)> {
    p.steps.iter().map(|s| (s.item.clone(), s.action.kind())).collect()
}

fn named(steps: &[(&str, &'static str)]) -> Vec<(String, &'static str)> {
    steps.iter().map(|(i, k)| ((*i).to_owned(), *k)).collect()
}

#[test]
fn a_bare_windows_turns_wsl_on_and_installs_docker_desktop_and_virtualbox() {
    let m = bare(
        HostOs::Windows,
        vec![status(Provider::Virtualbox, false, None), status(Provider::Hyperv, false, None)],
    );
    let p = plan_of(&m, &wanted(&[SetupPath::Docker, SetupPath::Vm], &[]));
    assert_eq!(steps(&p), named(&[("wsl", "install"), ("docker", "install"), ("virtualbox", "install")]));
    assert_eq!(p.hypervisor, Some(Provider::Virtualbox));
    assert_eq!(p.items, ["wsl", "docker", "engine:docker-desktop", "vagrant", "virtualbox"]);
}

#[test]
fn a_restart_for_wsl_ends_the_pass_and_docker_desktop_starts_only_after_it() {
    let mut m = bare(HostOs::Windows, Vec::new());
    m.wsl = Some(WslState::NeedsReboot);
    m.docker.installed = true;
    let p = plan_of(&m, &wanted(&[SetupPath::Docker], &[]));
    assert_eq!(steps(&p), named(&[("wsl", "restart")]));
    assert!(p.steps[0].action.ends_pass());
    m.docker.running = true;
    m.docker.engine = Some(Engine::DockerDesktop);
    assert!(options(&m, false).wsl.is_empty(), "a running engine is proof enough of WSL");
}

#[test]
fn a_linux_without_a_package_manager_leaves_each_install_to_its_user() {
    let mut m = bare(HostOs::Linux, vec![status(Provider::Libvirt, false, None)]);
    m.packages_installed = false;
    let o = options(&m, false);
    let docker = &o.engines.iter().find(|e| e.engine == Engine::Native).unwrap().actions[0];
    assert_eq!(
        docker,
        &Action::Manual {
            interactions: Vec::new(),
            reason: ManualReason::Distribution,
            dependency: Some(Dependency::Docker),
            url: Some("https://docs.docker.com/engine/install/".into()),
        }
    );
    assert_eq!(
        left_alone(docker),
        Some(LeftAlone::Distribution {
            dependency: Some(Dependency::Docker)
        })
    );
    let p = plan(&m, &o, &wanted(&[SetupPath::Docker, SetupPath::Vm], &[]));
    assert_eq!(p.steps[0].item, "docker");
    assert_eq!(p.hypervisor, None);
    assert_eq!(p.passed_over, [(Provider::Libvirt, PassedOver::Distribution)]);
}

#[test]
fn each_engine_says_what_makes_it_the_one_docker_uses() {
    let mut m = bare(HostOs::Linux, Vec::new());
    m.docker.installed = true;
    m.docker.denied = true;
    m.docker.engines_running = vec![Engine::Colima];
    let o = options(&m, false);
    let actions: Vec<&str> = o.engines.iter().map(|e| e.actions[0].kind()).collect();
    assert_eq!(actions, ["start", "relogin", "use"]);
    let p = plan(&m, &o, &wanted(&[SetupPath::Docker], &[]));
    assert_eq!(steps(&p), named(&[("docker", "relogin")]));
    m.docker.denied = false;
    m.docker.running = true;
    m.docker.engine = Some(Engine::Native);
    assert_eq!(options(&m, false).engines[1].actions, []);
    m.docker.installed = false;
    m.docker.running = false;
    m.docker.engines_running.clear();
    let vendor = &options(&m, false).engines[2].actions[0];
    assert_eq!(
        left_alone(vendor),
        Some(LeftAlone::LinksOut {
            url: Some(Engine::Colima.url().into())
        })
    );
}

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

#[test]
fn actions_are_tagged_by_kind() {
    let json = serde_json::to_value(Action::Install {
        dependency: Dependency::Docker,
        interactions: Vec::new(),
    })
    .unwrap();
    assert_eq!(json, serde_json::json!({ "kind": "install", "dependency": "docker", "interactions": [] }));
    assert_eq!(Options::empty().hypervisors, []);
}

/// A walk on a bare Linux whose runner installs Docker (then it runs) and fails anything else.
async fn walked(on_failure: OnFailure) -> (Walked, Vec<String>) {
    let mut libvirt = status(Provider::Libvirt, false, None);
    libvirt.needs = vec![Need::Vagrant];
    let start = bare(HostOs::Linux, vec![libvirt]);
    let current = std::cell::RefCell::new(start.clone());
    let mut told = Vec::new();
    let walked = walk(
        &wanted(&[SetupPath::Docker, SetupPath::Vm], &[]),
        (start.clone(), options(&start, false)),
        on_failure,
        Steps {
            run: async |step: &Step, line: &mut dyn FnMut(String)| {
                line(format!("running {}", step.item));
                if step.item == "docker" {
                    let mut now = current.borrow_mut();
                    now.docker.installed = true;
                    now.docker.running = true;
                    now.docker.engine = Some(Engine::Native);
                    true
                } else {
                    false
                }
            },
            failed: |ok: &bool| !ok,
            check: async || {
                let now = current.borrow().clone();
                let o = options(&now, false);
                (now, o)
            },
        },
        |event| {
            told.push(match event {
                Event::Started { item, kind, .. } => format!("{kind} {item}"),
                Event::Line { line, .. } => line,
                Event::Ended { item, outcome } => format!("{item} {outcome}"),
            });
        },
    )
    .await;
    (walked, told)
}

#[tokio::test]
async fn a_walk_runs_each_step_checks_again_and_goes_on_or_stops_after_a_failure() {
    let (on, told) = walked(OnFailure::Continue).await;
    assert_eq!(
        told,
        [
            "install docker",
            "running docker",
            "docker true",
            "install libvirt",
            "running libvirt",
            "libvirt false",
            "install vagrant",
            "running vagrant",
            "vagrant false"
        ]
    );
    assert_eq!(on.acted, ["docker", "libvirt", "vagrant"]);
    assert_eq!(on.failed.map(|s| s.item), Some("libvirt".into()));
    assert_eq!(on.ends, None);
    let (stopped, _) = walked(OnFailure::Stop).await;
    assert_eq!(stopped.acted, ["docker", "libvirt"]);
}

#[test]
fn a_vagrant_plugin_is_installed_only_by_a_plugins_name() {
    let step = crate::install::vagrant_plugin("vagrant-libvirt").unwrap();
    assert_eq!(crate::install::command_line(&step), "$ vagrant plugin install vagrant-libvirt");
    for not_a_name in ["--help", "vagrant-../x", "libvirt", &format!("vagrant-{}", "x".repeat(64))] {
        assert!(crate::install::vagrant_plugin(not_a_name).is_none(), "{not_a_name}");
    }
}

/// QEMU on Linux uses `/dev/kvm`, whose group an install joins the account to: a setup does
/// what it can now (Vagrant, the plugin) and asks for the new login last, also where the
/// hypervisor can't run yet for want of it (no `/dev/kvm` access in this session).
#[test]
fn qemus_kvm_group_joined_since_logging_in_asks_for_a_new_login_after_the_rest() {
    let mut qemu = status(Provider::Qemu, true, Some(Unavailable::NoKvm));
    qemu.plugin = Some("vagrant-qemu".into());
    qemu.plugin_installed = false;
    qemu.needs = vec![Need::Vagrant, Need::Plugin];
    let mut m = bare(HostOs::Linux, vec![qemu]);
    m.groups_pending = vec!["kvm".into()];
    let o = options(&m, false);
    assert_eq!(o.hypervisors[0].get, [Action::Relogin]);
    let p = plan(&m, &o, &wanted(&[SetupPath::Vm], &[Provider::Qemu]));
    assert_eq!(
        steps(&p),
        named(&[("vagrant", "install"), ("plugin:vagrant-qemu", "plugin"), ("qemu", "relogin")])
    );
    m.groups_pending = vec!["docker".into()];
    assert_ne!(options(&m, false).hypervisors[0].get, [Action::Relogin], "another tool's group");
}

#[test]
fn each_action_carries_what_it_asks_of_the_person_at_the_computer() {
    let mut m = bare(HostOs::Windows, vec![status(Provider::Virtualbox, false, None)]);
    let o = options(&m, false);
    let uac = Interaction::AdminPassword { via: Prompt::Uac };
    assert_eq!(o.wsl[0].interactions(), [uac, Interaction::Restart], "WSL's install, then its restart");
    assert_eq!(
        o.engines[0].actions[0].interactions()[1],
        Interaction::Licence {
            name: "Docker Subscription Service Agreement",
            url: "https://www.docker.com/legal/docker-subscription-service-agreement/",
        }
    );
    assert_eq!(Action::Relogin.interactions(), [Interaction::Relogin]);
    assert_eq!(
        Action::Restart {
            feature: WindowsFeature::Hyperv
        }
        .interactions(),
        [Interaction::Restart]
    );

    // Unattended on Linux: no password prompt, for an install or KVM's sharing.
    m = bare(
        HostOs::Linux,
        vec![status(Provider::Virtualbox, true, Some(Unavailable::KvmHoldsVirtualization))],
    );
    m.elevation = crate::install::Elevation::Unattended { root: false };
    let unattended = options(&m, false);
    assert_eq!(unattended.hypervisors[0].get, [Action::ShareVirtualization { interactions: Vec::new() }]);
    assert_eq!(
        unattended.engines[1].actions[0].interactions(),
        [Interaction::Relogin],
        "Docker Engine joins ada to docker"
    );
}

#[test]
fn a_hypervisor_or_engine_only_its_vendor_offers_is_a_download_by_hand_with_its_page() {
    use crate::install::{VMWARE_FUSION_PAGE, VMWARE_WORKSTATION_PAGE};
    for os in [HostOs::Windows, HostOs::Linux, HostOs::Macos] {
        let m = bare(os, vec![status(Provider::VmwareDesktop, false, None)]);
        let o = options(&m, false);
        let get = &o.hypervisors[0].get;
        assert_eq!(get.len(), 1, "{os:?}");
        assert_eq!(get[0].kind(), "manual", "{os:?}: never left out");
        let page = if os == HostOs::Macos { VMWARE_FUSION_PAGE } else { VMWARE_WORKSTATION_PAGE };
        assert_eq!(get[0].interactions(), [page.download()], "{os:?}");
        assert!(matches!(&get[0], Action::Manual { url: Some(u), .. } if u == page.url), "{os:?}");
    }
    // An engine a setup doesn't install: its own page.
    let m = bare(HostOs::Macos, Vec::new());
    let orbstack = options(&m, false).engines.into_iter().find(|e| e.engine == Engine::Orbstack).unwrap();
    assert_eq!(
        orbstack.actions[0].interactions(),
        [Interaction::DownloadByHand {
            vendor: "OrbStack",
            url: "https://orbstack.dev/"
        }]
    );
}
