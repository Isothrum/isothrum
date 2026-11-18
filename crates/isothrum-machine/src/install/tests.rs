use super::*;

#[test]
fn every_tool_runs_where_its_row_or_its_hypervisor_says() {
    let on = |os| Dependency::ALL.into_iter().filter(|d| d.runs_on(os)).collect::<Vec<_>>();
    assert!(!on(HostOs::Linux).contains(&Dependency::Utm) && !on(HostOs::Linux).contains(&Dependency::Wsl));
    assert!(on(HostOs::Macos).contains(&Dependency::Utm) && !on(HostOs::Macos).contains(&Dependency::Libvirt));
    assert!(on(HostOs::Windows).contains(&Dependency::Hyperv) && on(HostOs::Windows).contains(&Dependency::Wsl));
    for provider in [Provider::Virtualbox, Provider::Qemu, Provider::Utm, Provider::Libvirt, Provider::Hyperv] {
        assert_eq!(Dependency::of_hypervisor(provider).and_then(Dependency::hypervisor), Some(provider));
    }
    assert_eq!(
        Dependency::of_hypervisor(Provider::VmwareDesktop),
        None,
        "VMware's apps aren't installed by a setup"
    );
}

#[test]
fn each_tool_is_named_as_a_ui_names_it() {
    let names: Vec<serde_json::Value> = Dependency::ALL.iter().map(|d| serde_json::to_value(d).unwrap()).collect();
    assert_eq!(names[0], "docker");
    assert_eq!(names[11], "vmware-utility");
    assert_eq!(serde_json::from_value::<Dependency>(serde_json::json!("hyperv")).unwrap(), Dependency::Hyperv);
}

#[test]
fn each_elevation_runs_a_root_command_its_way() {
    let args = ["sh".to_owned(), "-c".to_owned(), "x".to_owned()];
    assert_eq!(
        Elevation::Prompt.as_root("env", &args),
        ("pkexec".to_owned(), ["env", "sh", "-c", "x"].map(String::from).to_vec())
    );
    assert_eq!(Elevation::Unattended { root: true }.as_root("env", &args), ("env".to_owned(), args.to_vec()));
    assert_eq!(Elevation::Unattended { root: false }.as_root("env", &args).1[..2], ["-n", "env"]);
}

#[tokio::test(start_paused = true)]
async fn a_tool_is_present_when_its_probe_answers() {
    use isothrum_runner::fake::{FakeProcesses, Reply};
    let tools = FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect("docker --version", Reply::missing())
        .build();
    let root = tempfile::tempdir().unwrap();
    let cx = Context::new(std::sync::Arc::new(tools), crate::testing::linux_host(root.path()));
    assert_eq!(present(&cx, Dependency::Vagrant).await, Some(true));
    assert_eq!(present(&cx, Dependency::Docker).await, Some(false));
    assert_eq!(present(&cx, Dependency::Wsl).await, None);
    assert_eq!(present(&cx, Dependency::VmwareUtility).await, Some(false), "its program isn't there");
}

#[tokio::test(start_paused = true)]
async fn a_hypervisor_is_present_as_its_providers_detection_reads_it() {
    use isothrum_runner::fake::{FakeProcesses, Reply};
    // qemu-img alone (qemu-utils, which many images carry) isn't QEMU: its emulator is, as the
    // provider's detection says. A setup that read qemu-img left QEMU "installed" and unusable.
    let tools = FakeProcesses::script()
        .expect("qemu-system-x86_64 --version", Reply::missing())
        .expect("virsh --version", Reply::ok("11.3.0"))
        .expect("VBoxManage --version", Reply::ok("7.2.4r170995"))
        .build();
    let root = tempfile::tempdir().unwrap();
    let cx = Context::new(std::sync::Arc::new(tools), crate::testing::linux_host(root.path()));
    for (dep, expected) in [
        (Dependency::Qemu, Some(false)),
        (Dependency::Libvirt, Some(true)),
        (Dependency::Virtualbox, Some(true)),
    ] {
        assert_eq!(present(&cx, dep).await, expected, "{dep:?}");
    }
}

/// A host on `os` (x86-64), its package manager `packages`, its devices under `root`.
fn host_on(os: HostOs, packages: Option<crate::host::Packages>, root: &std::path::Path) -> crate::host::Host {
    crate::host::Host {
        os,
        packages,
        ..crate::testing::linux_host(root)
    }
}

#[test]
fn a_plan_is_the_hosts_os_one_whatever_the_os_built_for() {
    use crate::host::{LinuxPackages, Packages};
    let root = tempfile::tempdir().unwrap();
    let mac = host_on(HostOs::Macos, Some(Packages::Homebrew("/opt/homebrew/bin/brew")), root.path());
    let programs = |steps: Vec<Step>| steps.into_iter().map(|s| s.program).collect::<Vec<_>>();
    assert_eq!(
        programs(plan(Dependency::Terraform, &mac, None, Elevation::Prompt).unwrap()),
        programs(macos::install(Dependency::Terraform, mac.packages, mac.arch).unwrap())
    );
    let windows = host_on(HostOs::Windows, Some(Packages::Winget), root.path());
    assert_eq!(programs(plan(Dependency::Vagrant, &windows, None, Elevation::Prompt).unwrap()), ["winget"]);
    let linux = host_on(HostOs::Linux, Some(Packages::Linux(LinuxPackages::Apt)), root.path());
    assert_eq!(programs(plan(Dependency::Docker, &linux, Some("ada"), Elevation::Prompt).unwrap()), ["pkexec"]);
    let unattended = plan(Dependency::Docker, &linux, Some("ada"), Elevation::Unattended { root: false }).unwrap();
    assert_eq!(programs(unattended), ["sudo"], "unattended, nobody answers a prompt");
}

#[test]
fn a_plan_is_refused_where_the_tool_doesnt_run_before_its_package_manager_is_read() {
    let root = tempfile::tempdir().unwrap();
    let linux = host_on(HostOs::Linux, None, root.path());
    assert_eq!(
        plan(Dependency::Utm, &linux, None, Elevation::Prompt).unwrap_err(),
        Refusal::NotHere {
            dependency: Dependency::Utm,
            os: HostOs::Linux
        }
    );
    assert_eq!(
        plan(Dependency::Vagrant, &linux, None, Elevation::Prompt).unwrap_err(),
        Refusal::WithoutPackages {
            dependency: Dependency::Vagrant
        }
    );
    let mac = host_on(HostOs::Macos, None, root.path());
    assert_eq!(plan(Dependency::Vagrant, &mac, None, Elevation::Prompt).unwrap_err(), Refusal::NoHomebrew);
}
