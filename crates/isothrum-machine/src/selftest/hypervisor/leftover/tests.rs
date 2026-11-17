use std::sync::Arc;

use isothrum_runner::ProcessRunner;
use isothrum_runner::fake::{FakeProcesses, Reply};

use super::*;
use crate::testing::linux_host;

fn cx(fake: FakeProcesses) -> Context {
    let runner: Arc<dyn ProcessRunner> = Arc::new(fake);
    Context::new(runner, linux_host(Path::new("/nonexistent/isothrum-machine-test")))
}

const VM: TestVm<'static> = TestVm {
    name: "Isothrum test VM",
    hostname: "isothrum-selftest",
};

#[tokio::test]
async fn the_tests_own_leftover_is_removed_and_anyone_elses_left_alone() {
    let dir = Path::new("/data/selftest/vm");
    let ours = format!("description=\"{}\"\n", vbox_description(&VM, dir));
    let fake = FakeProcesses::script()
        .allow("VBoxManage list vms", Reply::ok("\"Isothrum test VM\" {abc}\n\"other\" {def}\n"))
        .allow("VBoxManage showvminfo abc --machinereadable", Reply::ok(&ours))
        .allow("VBoxManage controlvm abc poweroff", Reply::fail(1, "not running"))
        .expect("VBoxManage unregistervm abc --delete", Reply::ok(""))
        .build();
    assert_eq!(clear(&cx(fake), dir, &VM).await, Ok(vec!["VirtualBox"]));

    let foreign = FakeProcesses::script()
        .allow("VBoxManage list vms", Reply::ok("\"Isothrum test VM\" {abc}\n"))
        .allow("VBoxManage showvminfo abc --machinereadable", Reply::ok("description=\"\"\n"))
        .build();
    assert_eq!(clear(&cx(foreign), dir, &VM).await, Err(Failure::Foreign { hypervisor: "VirtualBox" }));

    let none = FakeProcesses::script().allow("VBoxManage list vms", Reply::ok("\"stack · web\" {x}\n")).build();
    assert_eq!(clear(&cx(none), dir, &VM).await, Ok(Vec::new()));
    let no_vbox = FakeProcesses::script().allow("VBoxManage", Reply::fail(127, "not found")).build();
    assert_eq!(clear(&cx(no_vbox), dir, &VM).await, Ok(Vec::new()), "no VirtualBox here");
}

#[test]
fn a_leftover_hyperv_vm_is_removed_only_by_its_exact_name_with_its_disks_in_the_folder() {
    let script = hyperv_leftover_script("Isothrum test VM", &folder_expr(Path::new(r"C:\Users\jo\AppData\Local\app\selftest\vm\")));
    assert!(
        script.contains("Get-VM -Name 'Isothrum test VM'") && script.contains("$_.Name -ceq 'Isothrum test VM'"),
        "{script}"
    );
    // The folder with one separator: a sibling folder (`vm2`) doesn't count as under it.
    assert!(script.contains(r"StartsWith('C:\Users\jo\AppData\Local\app\selftest\vm\', "), "{script}");
    assert!(script.contains("if ($disks.Count -eq 0 -or $outside.Count -gt 0) { exit 20 }"), "{script}");
    let removal = script.find("Remove-VM").expect("a removal");
    assert!(
        script.find("exit 20").expect("a refusal") < removal,
        "every VM is checked before any is removed"
    );
    assert_eq!(hyperv_outcome(Some(0)), Ok(Leftover::None));
    assert_eq!(hyperv_outcome(Some(10)), Ok(Leftover::Removed));
    assert_eq!(hyperv_outcome(Some(20)), Ok(Leftover::Foreign));
    assert!(hyperv_outcome(Some(1223)).is_err_and(|e| e.to_string().contains("declined")));
    assert!(hyperv_outcome(Some(1)).is_err());
    assert_eq!(ps_quote("it's"), "'it''s'");
}
