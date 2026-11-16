//! The machine check as a launcher runs it, through this crate's public API only: a [`Context`]
//! on a host the test builds (its OS and CPU, its devices under a folder of its own:
//! [`Devices::under`]), its tools answered by a strict script (the runner's `fake` feature).

use std::path::Path;
use std::sync::Arc;

use isothrum_machine::providers::{self, Need, Provider, Unavailable, UtilityState};
use isothrum_machine::{Arch, Context, Devices, Host, HostOs};
use isothrum_runner::fake::{FakeProcesses, Reply};

/// The call that lists Vagrant's plugins.
const PLUGINS: &str = "vagrant plugin list --machine-readable";

/// A host on `os` and `arch` whose devices are under `root`: none there until a test makes it.
fn host(os: HostOs, arch: Arch, root: &Path) -> Host {
    Host {
        os,
        arch,
        packages: None,
        devices: Devices::under(root),
    }
}

/// Writes an empty file at `path`, its folders made.
fn file(path: &Path) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    std::fs::write(path, "")
}

/// The check on `host`, its tools answered by `tools`.
fn context(tools: FakeProcesses, host: Host) -> Context {
    Context::new(Arc::new(tools), host)
}

/// `vagrant plugin list --machine-readable`'s answer listing `plugins`, each at its version.
fn listing(plugins: &[(&str, &str)]) -> Reply {
    let lines: Vec<String> = plugins
        .iter()
        .flat_map(|(name, version)| {
            [
                format!("1,,plugin-name,{name}"),
                format!("1,{name},plugin-version,{version}%!(VAGRANT_COMMA) global"),
            ]
        })
        .collect();
    Reply::ok(&lines.join("\n"))
}

#[tokio::test(start_paused = true)]
async fn a_fresh_linux_machine_offers_its_providers_each_needing_vagrant_in_the_shape_a_ui_reads() {
    let root = tempfile::tempdir().expect("a root");
    let linux = host(HostOs::Linux, Arch::Amd64, root.path());
    file(&linux.devices.kvm).expect("the KVM device");
    // No hypervisor installed, and no group joined since logging in.
    let none = ["VBoxManage", "vmrun", "virsh", "qemu-system-x86_64", "id"];
    let tools = none
        .iter()
        .fold(FakeProcesses::script(), |script, tool| script.allow(tool, Reply::missing()))
        .build();
    let statuses = providers::detect(&context(tools, linux), false).await;
    let offered: Vec<Provider> = statuses.iter().map(|s| s.provider).collect();
    let expected = [
        Provider::Virtualbox,
        Provider::VmwareDesktop,
        Provider::Libvirt,
        Provider::Qemu,
        Provider::VmwareEsxi,
    ];
    assert_eq!(offered, expected);
    for s in &statuses {
        assert_eq!((s.available, &s.reason), (false, &Some(Unavailable::VagrantMissing)), "{s:?}");
        assert_eq!(s.needs.first(), Some(&Need::Vagrant), "{s:?}");
    }
    let libvirt = serde_json::to_value(&statuses[2]).expect("its JSON");
    assert_eq!(libvirt["provider"], "libvirt");
    assert_eq!(libvirt["reason"], serde_json::json!({ "code": "vagrantMissing" }));
    assert_eq!(libvirt["needs"], serde_json::json!(["vagrant", "plugin"]));
    assert_eq!(
        (libvirt["pluginInstalled"].as_bool(), libvirt["hypervisor"].as_bool()),
        (Some(false), Some(false))
    );
}

#[tokio::test]
async fn the_setup_plan_is_what_is_missing_in_install_order_and_a_status_carries_it() {
    assert_eq!(
        providers::needs(false, false, Some(UtilityState::Missing)),
        [Need::Vagrant, Need::Plugin, Need::VmwareUtility]
    );
    assert_eq!(providers::needs(true, true, Some(UtilityState::Running)), []);
    assert_eq!(providers::needs(true, false, None), [Need::Plugin]);

    let root = tempfile::tempdir().expect("a root");
    let tools = FakeProcesses::script()
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect(PLUGINS, listing(&[("vagrant-qemu", "0.8.0")]))
        .expect("vmrun", Reply::fail(255, "usage: vmrun [AUTHENTICATION-FLAGS] COMMAND [PARAMETERS]"))
        .build();
    let linux = host(HostOs::Linux, Arch::Amd64, root.path());
    let vmware = providers::status(&context(tools, linux), Provider::VmwareDesktop)
        .await
        .expect("offered on Linux");
    assert_eq!((vmware.hypervisor, vmware.utility), (Some(true), Some(UtilityState::Missing)));
    assert_eq!(
        vmware.needs,
        providers::needs(true, false, Some(UtilityState::Missing)),
        "the plan a setup installs"
    );
    assert_eq!(vmware.plugin.as_deref(), Some("vagrant-vmware-desktop"));
}

/// A Mac whose Vagrant has `plugin`, and whose hypervisor's tool answers `said` from its app only
/// (at `app_tool`): on `PATH`, it is missing. A model, as a strict script can't name a program
/// whose path has spaces (an app bundle's).
fn mac_with_app_tool(plugin: &'static str, app_tool: &Path, said: Reply) -> FakeProcesses {
    let app_tool = app_tool.display().to_string();
    FakeProcesses::model(move |call| match (call.program.as_str(), call.args.first().map(String::as_str)) {
        ("vagrant", Some("--version")) => Reply::ok("Vagrant 2.4.9"),
        ("vagrant", Some("plugin")) => listing(&[(plugin, "3.0.5")]),
        // As paths: one the code joins for a Mac has `/` where Windows would write `\\`.
        (program, _) if Path::new(program) == Path::new(&app_tool) => said.clone(),
        _ => Reply::missing(),
    })
}

/// The hypervisor's tool `provider` is found by on a Mac, found off `PATH` at `app_tool`, which
/// answers `said`: whether it reads installed, and the programs run.
async fn found_off_path(
    provider: Provider,
    plugin: &'static str,
    app_tool: &Path,
    said: Reply,
    root: &Path,
) -> Result<(Option<bool>, Vec<String>), Box<dyn std::error::Error>> {
    file(app_tool)?;
    let tools = Arc::new(mac_with_app_tool(plugin, app_tool, said));
    let cx = Context::new(Arc::<FakeProcesses>::clone(&tools), host(HostOs::Macos, Arch::Arm64, root));
    let status = providers::status(&cx, provider).await.ok_or("not offered on a Mac")?;
    Ok((status.hypervisor, tools.calls().iter().map(|c| c.program.clone()).collect()))
}

#[tokio::test]
async fn vmware_fusion_off_path_is_found_in_its_app() {
    let root = tempfile::tempdir().expect("a root");
    let vmrun = root.path().join("home/Applications/VMware Fusion.app/Contents/Public/vmrun");
    let usage = Reply::fail(255, "usage: vmrun [AUTHENTICATION-FLAGS] COMMAND [PARAMETERS]");
    let (installed, ran) = found_off_path(Provider::VmwareDesktop, "vagrant-vmware-desktop", &vmrun, usage, root.path())
        .await
        .expect("checked");
    assert_eq!(installed, Some(true), "{ran:?}");
    assert!(
        ran.contains(&"vmrun".to_owned()) && ran.iter().any(|r| Path::new(r) == vmrun),
        "PATH first, then the app's: {ran:?}"
    );
}

#[tokio::test]
async fn parallels_desktop_off_path_is_found_in_its_app() {
    let root = tempfile::tempdir().expect("a root");
    let prlctl = root.path().join("Applications/Parallels Desktop.app/Contents/MacOS/prlctl");
    let version = Reply::ok("prlctl version 20.1.0 (55732)");
    let (installed, ran) = found_off_path(Provider::Parallels, "vagrant-parallels", &prlctl, version, root.path())
        .await
        .expect("checked");
    assert_eq!(installed, Some(true), "{ran:?}");
    assert!(ran.iter().any(|r| Path::new(r) == prlctl), "{ran:?}");
}

/// A provider this host isn't offered (Hyper-V on Linux) is no status, and nothing is run for
/// it but Vagrant's probe.
#[tokio::test]
async fn a_provider_this_host_isnt_offered_is_never_checked() {
    let root = tempfile::tempdir().expect("a root");
    // Vagrant missing: nothing else may be asked.
    let nothing_asked = FakeProcesses::script().expect("vagrant --version", Reply::missing()).build();
    let linux = host(HostOs::Linux, Arch::Amd64, root.path());
    assert_eq!(providers::status(&context(nothing_asked, linux), Provider::Hyperv).await, None);
}
