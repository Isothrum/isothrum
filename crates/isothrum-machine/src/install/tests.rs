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
