use std::sync::Arc;

use isothrum_runner::fake::{FakeProcesses, Reply, Script};

use super::*;
use crate::host::{LinuxPackages, Packages};
use crate::testing::linux_host;

/// A Linux host's hypervisor probes, none installed, and Vagrant's plugins none.
fn no_hypervisors(script: Script) -> Script {
    ["VBoxManage", "vmrun", "virsh", "qemu-system-x86_64"]
        .iter()
        .fold(script, |script, tool| script.allow(tool, Reply::missing()))
}

/// The check on a Linux host with apt, its tools answered by `tools`.
fn on_linux(tools: FakeProcesses) -> Context {
    let mut host = linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.packages = Some(Packages::Linux(LinuxPackages::Apt));
    Context::new(Arc::new(tools), host)
}

/// A Debian machine with Docker Engine running, Docker's group joined since logging in, Vagrant
/// and no hypervisor: everything the check reports, read once.
#[tokio::test(start_paused = true)]
async fn a_linux_machine_is_checked_whole() {
    let tools = no_hypervisors(FakeProcesses::script())
        .expect("apt-get --version", Reply::ok("apt 3.0.3 (amd64)"))
        .expect("docker --version", Reply::ok("Docker version 28.5.1, build e180ab8"))
        .expect("docker compose version --short", Reply::ok("2.40.0"))
        .expect("docker info --format {{.OperatingSystem}}", Reply::ok("Debian GNU/Linux 13 (trixie)"))
        .expect("docker context show", Reply::ok("default"))
        .expect("docker context ls", Reply::ok("default\ttrue\n"))
        .expect("docker --context default info", Reply::ok("Debian GNU/Linux 13 (trixie)"))
        .expect("vagrant --version", Reply::ok("Vagrant 2.4.9"))
        .expect("vagrant plugin list --machine-readable", Reply::ok(""))
        .expect("terraform version", Reply::missing())
        .expect("aws --version", Reply::missing())
        .expect("az version --output json", Reply::missing())
        .expect("gcloud --version", Reply::ok("Google Cloud SDK 500.0.0"))
        .allow("id -un", Reply::ok("dev"))
        .allow("id -nG dev", Reply::ok("dev sudo docker"))
        .allow("id -nG", Reply::ok("dev sudo"))
        .build();
    let found = check(&on_linux(tools), Marks::default()).await;
    assert_eq!(
        (found.packages.name.as_str(), found.packages.version.as_deref()),
        ("apt", Some("apt 3.0.3 (amd64)"))
    );
    assert_eq!(found.docker.version.as_deref(), Some("Docker version 28.5.1, build e180ab8"));
    assert_eq!(found.compose.version.as_deref(), Some("2.40.0"));
    assert!(found.daemon.running() && !found.daemon.denied());
    assert_eq!(found.daemon.engine, Some(Engine::Native));
    assert_eq!(
        found.engines.iter().map(|e| (e.engine, e.running, e.current)).collect::<Vec<_>>(),
        [(Engine::Native, true, true)]
    );
    assert!(found.vagrant.installed && !found.terraform.installed);
    assert!(found.clouds.gcloud.installed && !found.clouds.aws.installed && !found.clouds.azure.installed);
    assert!(
        !found.providers.is_empty() && found.ready.is_empty(),
        "no hypervisor installed: {:?}",
        found.providers
    );
    assert!(found.wsl.is_none());
    assert_eq!(found.groups_pending, ["docker"]);
}

/// Nothing installed: a report all the same, every tool missing and the daemon down for want of
/// a CLI.
#[tokio::test(start_paused = true)]
async fn a_bare_machine_reads_as_nothing_installed() {
    let tools = FakeProcesses::model(|_| Reply::missing());
    let found = check(&on_linux(tools), Marks::default()).await;
    assert_eq!(
        (found.docker.clone(), found.compose.clone(), found.vagrant.clone()),
        (Tool::MISSING, Tool::MISSING, Tool::MISSING)
    );
    assert_eq!(
        found.daemon,
        Daemon {
            answer: Err(NoAnswer::NoCli),
            engine: None
        }
    );
    assert!(found.engines.is_empty() && found.ready.is_empty() && found.groups_pending.is_empty());
    assert!(!found.packages.installed);
}

#[tokio::test(start_paused = true)]
async fn a_daemon_that_refuses_this_account_is_denied() {
    let tools = FakeProcesses::model(|call| match (call.program.as_str(), call.args.first().map(String::as_str)) {
        ("docker", Some("info")) => Reply::fail(1, "permission denied while trying to connect to the docker API at unix:///var/run/docker.sock"),
        ("docker", Some("--version")) => Reply::ok("Docker version 26.1.5"),
        _ => Reply::missing(),
    });
    let found = check(&on_linux(tools), Marks::default()).await;
    assert!(found.docker.installed && !found.daemon.running() && found.daemon.denied());
    assert_eq!(found.daemon.engine, None);
}

#[test]
fn only_available_local_providers_with_vagrant_are_ready() {
    let status = |provider, remote, available| {
        let mut json = serde_json::json!({
            "provider": provider, "remote": remote, "available": available, "hypervisor": null, "plugin": null,
            "pluginInstalled": true, "utility": null, "needs": [], "reason": null,
        });
        json["accountLacks"] = serde_json::Value::Null;
        serde_json::from_value::<ProviderStatus>(json).unwrap()
    };
    let statuses = [
        status("virtualbox", false, true),
        status("libvirt", false, false),
        status("vmware_esxi", true, true),
    ];
    assert_eq!(ready(&statuses, true), [Provider::Virtualbox]);
    assert_eq!(ready(&statuses, false), []);
}
