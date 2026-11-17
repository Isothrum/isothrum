//! The ESXi course: the box named with the architecture of its build, its size read from the box
//! store, the failures people hit on their step, and the Vagrantfile.

use super::*;

/// A fresh box store with box `bento/debian-12` v202510.26.0's builds `arches` (each
/// `(arch, provider, bytes)`).
fn store(arches: &[(&str, &str, usize)]) -> tempfile::TempDir {
    let boxes = tempfile::tempdir().unwrap();
    let version = boxes.path().join("bento-VAGRANTSLASH-debian-12").join("202510.26.0");
    for (arch, provider, bytes) in arches {
        let build = version.join(arch).join(provider);
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("disk-s001.vmdk"), vec![0_u8; *bytes]).unwrap();
    }
    boxes
}

const SUMMARY: [&str; 2] = [
    "==> default: --- Box             : bento/debian-12",
    "==> default: --- Box Ver         : 202510.26.0",
];

fn named(arch: Option<&str>) -> Found {
    Found::Box {
        name: "bento/debian-12".into(),
        version: Some("202510.26.0".into()),
        arch: arch.map(str::to_owned),
    }
}

#[test]
fn a_box_already_downloaded_is_named_with_the_architecture_vagrant_stored_it_for() {
    let boxes = store(&[("arm64", "vmware_desktop", 5), ("amd64", "virtualbox", 5)]);
    let mut e = OnEsxi::new(Some(boxes.path().to_path_buf()));
    assert_eq!(e.found(0), Found::Nothing, "no box named yet");
    // No `Adding box` line: the plugin's summary names the box, the store its architecture.
    for line in SUMMARY {
        assert_eq!(e.ends(line), None);
    }
    assert_eq!(e.found(0), named(Some("arm64")), "only the VMware build counts");
    std::fs::create_dir_all(boxes.path().join("bento-VAGRANTSLASH-debian-12/202510.26.0/amd64/vmware_desktop")).unwrap();
    assert_eq!(e.found(0), named(Some("amd64 or arm64")));
    let path = boxes.path().to_path_buf();
    drop(boxes);
    assert!(!path.exists());
    assert_eq!(e.found(0), named(None), "architecture unknown");
}

#[test]
fn the_upload_carries_the_size_of_the_box_build_it_uploads_once_stored() {
    let boxes = store(&[("amd64", "vmware_desktop", 3000), ("arm64", "vmware_desktop", 5), ("amd64", "virtualbox", 900)]);
    let mut e = OnEsxi::new(Some(boxes.path().to_path_buf()));
    e.ends("==> default: Adding box 'bento/debian-12' (v202510.26.0) for provider: vmware_desktop (amd64)");
    assert_eq!(e.bytes(0), Some(3000), "the amd64 VMware build Vagrant added, not another");
    assert_eq!(e.bytes(1), None);
    assert_eq!(e.found(0), named(Some("amd64")));
    // Not stored (yet): no size; with no architecture named, the only VMware build stored.
    assert_eq!(stored_size(boxes.path(), "bento/debian-12", "202510.1.0", None), None);
    std::fs::remove_dir_all(boxes.path().join("bento-VAGRANTSLASH-debian-12/202510.26.0/arm64")).unwrap();
    assert_eq!(stored_size(boxes.path(), "bento/debian-12", "202510.26.0", None), Some(3000));
}

#[test]
fn each_step_ends_on_the_plugins_line() {
    let mut e = OnEsxi::new(None);
    assert_eq!(e.ends("==> default: --- VMID            : 12"), Some(0));
    assert_eq!(e.ends("==> default: --- VM has been Powered On..."), Some(1));
    assert_eq!(e.ends("==> default: --- Waiting for state \"running\""), None);
    assert_eq!(e.ends("==> default: --- Success, state is now \"running\""), Some(2));
    assert_eq!(e.machine(), "default");
}

#[test]
fn the_failures_people_hit_belong_to_their_step() {
    let failed = |message: &str| ErrorReport::new(isothrum_runner::ErrorCode::ToolFailed, message);
    let e = OnEsxi::new(None);
    let at = |message: &str| e.explain(&failed(message)).map(|(at, _)| at);
    assert!(matches!(e.explain(&failed("Error: Duplicate name 'test-vm'")), Some((0, Failure::NameTaken))));
    assert_eq!(at("The box 'bento/debian-12' could not be found or could not be accessed"), Some(0));
    assert_eq!(at("There was an error talking to ESXi.\n  Unable to power on VM:\n  …"), Some(1));
    assert!(matches!(
        e.explain(&failed("There was an error talking to ESXi.\n  Failed, timeout waiting for \"running\".")),
        Some((2, Failure::NoAddress(_)))
    ));
    assert!(matches!(
        e.explain(&failed("Timed out while waiting for the machine to boot.")),
        Some((3, Failure::NoSsh(_)))
    ));
    assert_eq!(at("Permission denied (publickey)"), None);
}

#[test]
fn a_failed_upload_says_the_ovf_tools_own_error_line() {
    let mut e = OnEsxi::new(None);
    e.ends("Opening VMX source: /tmp/default.vmx");
    let report = ErrorReport::new(isothrum_runner::ErrorCode::ToolFailed, "There was an OVF Tool error.");
    assert!(matches!(e.explain(&report), Some((0, Failure::Upload(said))) if said == "There was an OVF Tool error."));
    e.ends("Error: No space left on device");
    assert!(matches!(e.explain(&report), Some((0, Failure::Upload(said))) if said == "Error: No space left on device"));
    // The OVF Tool's line naming a VM of that name already there is the name taken.
    e.ends("Error: Duplicate name 'test-vm'");
    assert!(matches!(e.explain(&report), Some((0, Failure::NameTaken))));
}

#[test]
fn the_vagrantfile_names_the_vm_and_the_host_and_reads_the_password_from_the_run() {
    let mut esxi = Esxi::new("esxi.lan", "root", SecretValue::new("pw'd"));
    esxi.port = 2222;
    esxi.network = Some("Test Net".into());
    let (file, text) = esxi.project(&TestVm::new("vm-'quoted'", "probe"));
    assert_eq!(file, "Vagrantfile");
    for line in [
        "config.vm.box = \"bento/debian-12\"",
        "config.vm.box_architecture = \"amd64\"",
        "config.vm.provider \"vmware_esxi\" do |v|",
        "v.esxi_hostname = 'esxi.lan'",
        "v.esxi_hostport = 2222",
        "v.esxi_password = \"env:ESXI_PASSWORD\"",
        "v.guest_name = 'vm-\\'quoted\\''",
        "v.esxi_virtual_network = ['Test Net']",
    ] {
        assert!(text.contains(line), "{line} missing from\n{text}");
    }
    assert!(!text.contains("esxi_disk_store"), "the plugin picks the datastore when none is given");
    assert!(!text.contains("pw'd"));
    assert_eq!(esxi.options().secrets.get(PASSWORD).map(SecretValue::expose), Some("pw'd"));
    assert_eq!(esxi.options().provider, Some(Provider::VmwareEsxi));
    assert_eq!(esxi.ssh_address(), "esxi.lan:2222");
    assert_eq!(Esxi::new("fd00::7", "root", SecretValue::new("x")).ssh_address(), "[fd00::7]:22");
}
