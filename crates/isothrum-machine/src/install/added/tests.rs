use std::sync::{Arc, Mutex};

use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::host::Packages;
use crate::install::{Elevation, Refusal, removal};

/// A Debian host whose dpkg lists `packages` (shared: a test installs into it), each installed
/// (`ii`), beside one only its configuration files are left of (`rc`).
fn debian(packages: &Arc<Mutex<Vec<&'static str>>>) -> Context {
    let listed = Arc::clone(packages);
    let tools = FakeProcesses::model(move |call| {
        if call.program == "dpkg-query" {
            let mut out = String::from("rc \tqemu-system-ppc\n");
            for p in listed.lock().unwrap().iter() {
                out.extend(["ii \t", p, "\n"]);
            }
            Reply::ok(&out)
        } else {
            Reply::missing()
        }
    });
    let mut host = crate::testing::linux_host(std::path::Path::new("/nonexistent/isothrum-machine-test"));
    host.packages = Some(Packages::Linux(LinuxPackages::Apt));
    Context::new(Arc::new(tools), host)
}

fn package(name: &str) -> Item {
    Item::Package(name.to_owned())
}

/// What QEMU's install on Debian brings, its metapackage's dependencies with it.
const QEMU: [&str; 5] = ["qemu-system", "qemu-system-x86", "qemu-utils", "genisoimage", "virtiofsd"];

#[tokio::test]
async fn a_tool_that_was_missing_records_everything_its_install_added_of_it() {
    let dpkg = Arc::new(Mutex::new(vec!["bash", "libc6"]));
    let cx = debian(&dpkg);
    let before = before(&cx, Dependency::Qemu).await;
    dpkg.lock().unwrap().extend(QEMU.iter().chain(&["libslirp0"]));
    let installed = before.installed(&cx).await;
    assert_eq!(
        installed.added,
        ["genisoimage", "qemu-system", "qemu-system-x86", "qemu-utils", "virtiofsd"].map(package),
        "a dependency of no tool's (libslirp0) isn't QEMU's to remove"
    );
    assert_eq!(installed.waiting, []);
}

#[tokio::test]
async fn a_tool_there_without_its_helpers_records_only_the_helpers_its_install_added() {
    // QEMU installed by hand, its VMs' helpers missing: a setup installs it again, which brings
    // them, and only they are the install's.
    let dpkg = Arc::new(Mutex::new(vec!["qemu-system-x86", "qemu-utils", "qemu-system"]));
    let cx = debian(&dpkg);
    let before = before(&cx, Dependency::Qemu).await;
    dpkg.lock().unwrap().extend(["genisoimage", "virtiofsd"]);
    let installed = before.installed(&cx).await;
    assert_eq!(installed.added, [package("genisoimage"), package("virtiofsd")]);

    let mut tools = Vec::new();
    super::super::record::remember(&mut tools, Dependency::Qemu, &installed, 1);
    let removable = super::super::record::removable(tools);
    let steps = removal(Dependency::Qemu, &cx.host, &removable[0].added, Elevation::Unattended { root: true }, None).unwrap();
    let script = &steps[0].args[2];
    assert!(script.ends_with("apt-get remove -y genisoimage virtiofsd"), "{script}");
    assert!(!script.contains("qemu"), "QEMU itself stays: {script}");
}

#[tokio::test]
async fn an_install_that_added_nothing_records_nothing_nor_one_whose_packages_cant_be_read() {
    let dpkg = Arc::new(Mutex::new(QEMU.to_vec()));
    let cx = debian(&dpkg);
    assert_eq!(before(&cx, Dependency::Qemu).await.installed(&cx).await, Installed::default());

    let unread = Context::new(Arc::new(FakeProcesses::model(|_| Reply::fail(2, "dpkg: error"))), cx.host.clone());
    assert_eq!(before(&unread, Dependency::Qemu).await.installed(&cx).await, Installed::default());
}

#[test]
fn a_removal_never_takes_a_package_the_record_cant_prove_its_installs_added() {
    let mut host = crate::testing::linux_host(std::path::Path::new("/nonexistent"));
    host.packages = Some(Packages::Linux(LinuxPackages::Apt));
    let remove = |added: &[Item]| removal(Dependency::Qemu, &host, added, Elevation::Unattended { root: true }, None);
    // Nothing on record, a package of another tool's, or one that isn't a package's name.
    for added in [
        vec![],
        vec![package("bash")],
        vec![package("qemu-system; rm -rf /")],
        vec![Item::Tool(Dependency::Qemu)],
    ] {
        assert_eq!(remove(&added).unwrap_err(), Refusal::NothingAdded { dependency: Dependency::Qemu }, "{added:?}");
    }
    let script = remove(&[package("bash"), package("qemu-system-arm")]).unwrap()[0].args[2].clone();
    assert!(script.ends_with("apt-get remove -y qemu-system-arm"), "{script}");
    // apt would take away what needs it too: checked first, and nothing removed then.
    assert!(script.contains("apt-get -s remove qemu-system-arm"), "{script}");
}

#[test]
fn elsewhere_a_tool_is_removed_only_when_its_install_added_it() {
    let mac = crate::host::Host {
        os: HostOs::Macos,
        packages: Some(Packages::Homebrew("/opt/homebrew/bin/brew")),
        ..crate::testing::linux_host(std::path::Path::new("/nonexistent"))
    };
    assert_eq!(items(Dependency::Terraform, &mac), [Item::Tool(Dependency::Terraform)]);
    let remove = |added: &[Item]| removal(Dependency::Terraform, &mac, added, Elevation::Prompt, None);
    assert_eq!(
        remove(&[]).unwrap_err(),
        Refusal::NothingAdded {
            dependency: Dependency::Terraform
        }
    );
    assert_eq!(
        remove(&[Item::Tool(Dependency::Qemu)]).unwrap_err(),
        Refusal::NothingAdded {
            dependency: Dependency::Terraform
        }
    );
    assert_eq!(
        remove(&[Item::Tool(Dependency::Terraform)]).unwrap()[0].args,
        ["uninstall", "hashicorp/tap/terraform"]
    );
}

#[test]
fn each_hypervisors_packages_are_its_own_and_its_helpers() {
    for manager in [LinuxPackages::Apt, LinuxPackages::Dnf] {
        for dep in [Dependency::Qemu, Dependency::Libvirt] {
            let own = packages(dep, manager);
            for helper in crate::providers::helpers(dep.hypervisor().unwrap(), HostOs::Linux) {
                assert!(own.iter().any(|p| p == helper.package(manager)), "{dep:?} through {manager:?}: {own:?}");
            }
        }
    }
    assert!(matches("qemu-system*", "qemu-system-x86") && matches("virtiofsd", "virtiofsd") && !matches("virtiofsd", "virtiofsd-extra"));
}

#[test]
fn the_vmware_utility_s_install_on_apt_records_the_net_tools_it_brings() {
    // Its install brings net-tools for the plugin's `netstat`: unrecorded, a clean-up left it,
    // and a machine without it read the install as having added nothing.
    assert_eq!(packages(Dependency::VmwareUtility, LinuxPackages::Apt), ["vagrant-vmware-utility", "net-tools"]);
}
