use std::sync::Arc;

use isothrum_runner::fake::{Call, FakeProcesses, Reply};
use isothrum_runner::{Cloud, Detail, Error, Format, HostOs};

use super::*;
use crate::host::{Host, LinuxPackages, Packages};

/// A tool.
fn tool(dependency: Dependency) -> Requirement {
    Requirement::Tool { dependency }
}

/// A Linux host with apt, its devices under `root`.
fn apt_host(root: &Path) -> Host {
    Host {
        packages: Some(Packages::Linux(LinuxPackages::Apt)),
        ..crate::testing::linux_host(root)
    }
}

/// A context on `host` whose tools answer as `answer` says, each call kept.
fn on(host: Host, answer: impl Fn(&Call) -> Reply + Send + Sync + 'static) -> (Context, Arc<FakeProcesses>) {
    let fake = Arc::new(FakeProcesses::model(answer));
    {
        let runner: Arc<dyn isothrum_runner::ProcessRunner> = Arc::<FakeProcesses>::clone(&fake);
        (Context::new(runner, host), fake)
    }
}

#[test]
fn a_compose_run_needs_the_engine_its_plugin_and_the_engine_answering() {
    assert_eq!(
        Uses::new(Format::Compose).requirements(),
        [tool(Dependency::Docker), Requirement::ComposePlugin, Requirement::EngineRunning]
    );
}

#[test]
fn a_vagrant_run_needs_vagrant_its_hypervisor_and_what_vagrant_drives_it_through() {
    assert_eq!(
        Uses::vagrant(Provider::Virtualbox).requirements(),
        [
            tool(Dependency::Vagrant),
            Requirement::Hypervisor {
                provider: Provider::Virtualbox
            }
        ],
        "VirtualBox is built into Vagrant"
    );
    assert_eq!(
        Uses::vagrant(Provider::Libvirt).requirements(),
        [
            tool(Dependency::Vagrant),
            Requirement::Hypervisor { provider: Provider::Libvirt },
            Requirement::VagrantPlugin { provider: Provider::Libvirt }
        ]
    );
    assert_eq!(
        Uses::vagrant(Provider::VmwareDesktop).requirements(),
        [
            tool(Dependency::Vagrant),
            Requirement::Hypervisor {
                provider: Provider::VmwareDesktop
            },
            Requirement::VagrantPlugin {
                provider: Provider::VmwareDesktop
            },
            tool(Dependency::VmwareUtility)
        ]
    );
    assert_eq!(
        Uses::new(Format::Vagrant).requirements(),
        [tool(Dependency::Vagrant)],
        "no hypervisor named yet"
    );
}

#[test]
fn an_esxi_run_needs_no_hypervisor_here_but_its_plugin_and_the_ovf_tool() {
    assert_eq!(
        Uses::vagrant(Provider::VmwareEsxi).requirements(),
        [
            tool(Dependency::Vagrant),
            Requirement::VagrantPlugin {
                provider: Provider::VmwareEsxi
            },
            Requirement::Ovftool
        ]
    );
}

#[test]
fn a_terraform_run_needs_terraform_and_its_targets_cli_or_the_ovf_tool() {
    let cloud = |cloud| Uses::terraform(Target::Cloud { cloud }).requirements();
    assert_eq!(cloud(Cloud::Aws), [tool(Dependency::Terraform), tool(Dependency::Awscli)]);
    assert_eq!(cloud(Cloud::Azure), [tool(Dependency::Terraform), tool(Dependency::Azurecli)]);
    assert_eq!(cloud(Cloud::Gcp), [tool(Dependency::Terraform), tool(Dependency::Gcloud)]);
    for keyed in [Cloud::Digitalocean, Cloud::Linode, Cloud::Oci] {
        assert_eq!(cloud(keyed), [tool(Dependency::Terraform)], "{keyed:?} signs in with a token or keys");
    }
    assert_eq!(Uses::terraform(Target::Proxmox).requirements(), [tool(Dependency::Terraform)]);
    assert_eq!(
        Uses::terraform(Target::Esxi).requirements(),
        [tool(Dependency::Terraform), Requirement::Ovftool]
    );
    assert_eq!(Uses::new(Format::Terraform).requirements(), [tool(Dependency::Terraform)]);
}

#[test]
fn a_provider_or_target_the_format_doesnt_use_is_ignored() {
    let compose = Uses::new(Format::Compose).on(Provider::Libvirt).at(Target::Esxi);
    assert_eq!(compose.requirements(), Uses::new(Format::Compose).requirements());
    let vagrant = Uses::vagrant(Provider::Virtualbox).at(Target::Esxi);
    assert_eq!(vagrant.requirements(), Uses::vagrant(Provider::Virtualbox).requirements());
}

#[test]
fn a_requirement_is_named_as_a_ui_reads_it() {
    let json = |r: Requirement| serde_json::to_value(r).unwrap();
    assert_eq!(json(tool(Dependency::Vagrant)), serde_json::json!({"kind": "tool", "dependency": "vagrant"}));
    assert_eq!(json(Requirement::ComposePlugin), serde_json::json!({"kind": "composePlugin"}));
    assert_eq!(
        json(Requirement::VagrantPlugin {
            provider: Provider::VmwareEsxi
        }),
        serde_json::json!({"kind": "vagrantPlugin", "provider": "vmware_esxi"})
    );
    let back: Requirement = serde_json::from_value(serde_json::json!({"kind": "ovftool"})).unwrap();
    assert_eq!(back, Requirement::Ovftool);
}

#[test]
fn a_missing_tool_the_runner_names_is_its_requirement() {
    let missing = |tool: &str| Error::ToolMissing {
        step: "up".to_owned(),
        tool: tool.to_owned(),
    };
    assert_eq!(Requirement::of_error(&missing("docker")), Some(tool(Dependency::Docker)));
    assert_eq!(Requirement::of_error(&missing("vagrant")), Some(tool(Dependency::Vagrant)));
    assert_eq!(Requirement::of_error(&missing("terraform.exe")), Some(tool(Dependency::Terraform)));
    assert_eq!(Requirement::of_error(&missing("ovftool")), Some(Requirement::Ovftool));
    assert_eq!(
        Requirement::of_error(&missing("VBoxManage")),
        Some(Requirement::Hypervisor {
            provider: Provider::Virtualbox
        })
    );
    assert_eq!(Requirement::of_error(&missing("ssh")), None, "nothing a setup installs");
    let unanswered = Error::ToolUnanswered {
        step: "tools".to_owned(),
        tool: "docker".to_owned(),
        reason: "Cannot connect to the Docker daemon".to_owned(),
    };
    assert_eq!(Requirement::of_error(&unanswered), Some(Requirement::EngineRunning));
    assert_eq!(Requirement::of_error(&Error::Locked { run: "a".to_owned() }), None, "not a missing requirement");
    let detail = Detail::ToolMissing { tool: "aws".to_owned() };
    assert_eq!(Requirement::of_detail(&detail), Some(tool(Dependency::Awscli)));
}

#[tokio::test(start_paused = true)]
async fn present_tools_have_no_plan_and_missing_ones_the_hosts() {
    let root = tempfile::tempdir().unwrap();
    let (cx, fake) = on(apt_host(root.path()), |call| match call.line().as_str() {
        "docker --version" => Reply::ok("Docker version 27.3.1"),
        "docker compose version --short" => Reply::ok("2.29.7"),
        _ => Reply::missing(),
    });
    let needs = needs(&cx, &Uses::new(Format::Compose).requirements(), &Asking::default()).await;
    let [docker, compose, engine] = needs.as_slice() else { panic!("{needs:?}") };
    assert_eq!((docker.present, compose.present), (Some(true), Some(true)));
    assert!(docker.plan.is_none() && compose.plan.is_none(), "{needs:?}");
    assert!(engine.missing());
    assert!(matches!(engine.plan, Some(Plan::Start)), "{engine:?}");
    let ran: Vec<String> = fake.calls().iter().map(Call::line).collect();
    assert!(
        ran.iter().all(|l| !l.contains("install") && !l.starts_with("pkexec") && !l.starts_with("sudo")),
        "asking installs nothing: {ran:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_missing_tool_is_planned_as_its_install_here() {
    let root = tempfile::tempdir().unwrap();
    let (cx, _fake) = on(apt_host(root.path()), |_| Reply::missing());
    let asking = Asking {
        user: Some("ada"),
        elevation: Elevation::Unattended { root: false },
        ..Asking::default()
    };
    let found = needs(&cx, &[tool(Dependency::Terraform), Requirement::ComposePlugin], &asking).await;
    for need in &found {
        assert!(need.missing(), "{need:?}");
        let Some(Plan::Install(steps)) = &need.plan else { panic!("{need:?}") };
        assert_eq!(steps.iter().map(|s| s.program.as_str()).collect::<Vec<_>>(), ["sudo"], "{need:?}");
    }
}

#[tokio::test(start_paused = true)]
async fn a_tool_without_packages_here_is_refused_with_why() {
    let root = tempfile::tempdir().unwrap();
    let (cx, _fake) = on(crate::testing::linux_host(root.path()), |_| Reply::missing());
    let found = needs(&cx, &[tool(Dependency::Vagrant)], &Asking::default()).await;
    assert!(
        matches!(
            &found[0].plan,
            Some(Plan::Refused(Refusal::WithoutPackages {
                dependency: Dependency::Vagrant
            }))
        ),
        "{found:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_vagrant_runs_needs_read_its_providers_status_once() {
    let root = tempfile::tempdir().unwrap();
    let (cx, fake) = on(apt_host(root.path()), |call| match call.line().as_str() {
        "vagrant --version" => Reply::ok("Vagrant 2.4.9"),
        "vagrant plugin list --machine-readable" => Reply::ok(""),
        _ => Reply::missing(),
    });
    let found = needs(&cx, &Uses::vagrant(Provider::Libvirt).requirements(), &Asking::default()).await;
    let [vagrant, hypervisor, plugin] = found.as_slice() else {
        panic!("{found:?}")
    };
    assert_eq!(vagrant.present, Some(true));
    assert!(hypervisor.missing(), "{hypervisor:?}");
    assert!(hypervisor.unavailable.is_some(), "its detection says why: {hypervisor:?}");
    assert!(vagrant.unavailable.is_none() && plugin.unavailable.is_none(), "a hypervisor's only");
    assert!(matches!(&hypervisor.plan, Some(Plan::Install(_))), "libvirt's own install: {hypervisor:?}");
    assert!(plugin.missing(), "{plugin:?}");
    let Some(Plan::Install(steps)) = &plugin.plan else { panic!("{plugin:?}") };
    assert_eq!(install::command_line(&steps[0]), "$ vagrant plugin install vagrant-libvirt");
    assert_eq!(fake.made("vagrant plugin list --machine-readable").len(), 1, "one status for both");
}

#[tokio::test(start_paused = true)]
async fn a_hypervisor_no_setup_installs_is_its_vendors_to_get() {
    let root = tempfile::tempdir().unwrap();
    let host = Host {
        os: HostOs::Macos,
        packages: Some(Packages::Homebrew("/opt/homebrew/bin/brew")),
        ..crate::testing::linux_host(root.path())
    };
    let (cx, _fake) = on(host, |_| Reply::missing());
    let plan = plan(&cx, Requirement::Hypervisor { provider: Provider::Parallels }, &Asking::default());
    assert!(matches!(plan, Plan::ByHand { url: Some(u) } if u.contains("parallels")), "{plan:?}");
}

#[tokio::test(start_paused = true)]
async fn the_ovf_tool_is_found_on_the_path_given_else_fetched_by_hand() {
    let root = tempfile::tempdir().unwrap();
    let (cx, _fake) = on(apt_host(root.path()), |_| Reply::missing());
    let missing = needs(&cx, &[Requirement::Ovftool], &Asking::default()).await;
    assert!(missing[0].missing());
    assert!(matches!(missing[0].plan, Some(Plan::ByHand { url: Some(OVFTOOL_URL) })), "{missing:?}");

    let bin = root.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("ovftool"), "#!/bin/sh\n").unwrap();
    let path = std::env::join_paths([&bin]).unwrap();
    let asking = Asking {
        path: Some(&path),
        ..Asking::default()
    };
    let found = needs(&cx, &[Requirement::Ovftool], &asking).await;
    assert_eq!(found[0].present, Some(true), "{found:?}");
    assert!(found[0].plan.is_none());
}

/// Regression: VMware's utility was planned as an install on a computer without VMware, so a run
/// that installs what it needs ran it first: its Windows installer exits 1603 ("requires a valid
/// installation of VMware Workstation"), and on Linux its service never answers. It comes after
/// VMware, which its user gets from its vendor.
#[tokio::test(start_paused = true)]
async fn vmwares_utility_comes_after_vmware_which_its_user_gets_first() {
    let root = tempfile::tempdir().unwrap();
    let (cx, _fake) = on(apt_host(root.path()), |call| match call.line().as_str() {
        "vagrant --version" => Reply::ok("Vagrant 2.4.9"),
        "vagrant plugin list --machine-readable" => Reply::ok(""),
        _ => Reply::missing(),
    });
    let found = needs(&cx, &Uses::vagrant(Provider::VmwareDesktop).requirements(), &Asking::default()).await;
    let utility = found.iter().find(|n| n.requirement == tool(Dependency::VmwareUtility)).unwrap();
    assert!(utility.missing(), "{utility:?}");
    assert!(
        matches!(
            utility.plan,
            Some(Plan::After {
                requirement: Requirement::Hypervisor {
                    provider: Provider::VmwareDesktop
                }
            })
        ),
        "{utility:?}"
    );
    let vmware = found
        .iter()
        .find(|n| {
            n.requirement
                == Requirement::Hypervisor {
                    provider: Provider::VmwareDesktop,
                }
        })
        .unwrap();
    assert!(matches!(vmware.plan, Some(Plan::ByHand { .. })), "VMware from its vendor first: {vmware:?}");
    assert_eq!(Dependency::VmwareUtility.serves(), Some(Provider::VmwareDesktop));
    assert!(Dependency::ALL.iter().all(|d| *d == Dependency::VmwareUtility || d.serves().is_none()));
}
