use super::*;
use crate::cli::testing;
use crate::process::Exit;
use crate::process::fake::{FakeProcesses, Reply};

fn lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

#[test]
fn states_read_as_virtualbox_prints_them() {
    assert_eq!(
        vm_state_of(&lines(include_str!("../../tool_output/fixtures/vboxmanage/7.0-showvminfo-aborted-saved.txt"))),
        Some(VmState::AbortedSaved)
    );
    assert_eq!(
        vm_state_of(&lines(include_str!("../../tool_output/fixtures/vboxmanage/7.0.20-showvminfo-vrde-off.txt"))),
        Some(VmState::Other("poweroff".to_owned()))
    );
    assert_eq!(vm_state_of(&lines("name=\"x\"\r\n")), None);
}

#[test]
fn a_vm_virtualbox_doesnt_have_is_not_found() {
    let said = Captured::new(
        TOOL,
        Exit::new(Some(1)),
        Vec::new(),
        lines(include_str!("../../tool_output/fixtures/vboxmanage/7.0-showvminfo-not-found.txt")),
    );
    assert_eq!(Failure::of(&said), Failure::NotFound);
    let other = Captured::new(TOOL, Exit::new(Some(1)), Vec::new(), lines("VBoxManage: error: The machine is locked"));
    assert_eq!(
        Failure::of(&other),
        Failure::Other("VBoxManage: error: The machine is locked (exited with code 1)".to_owned())
    );
}

#[tokio::test]
async fn listings_read_as_virtualbox_prints_them() {
    let fake = FakeProcesses::script()
        .expect(
            "VBoxManage list vms",
            Reply::ok(include_str!("../../tool_output/fixtures/vboxmanage/7.0.20-list-vms.txt")),
        )
        .expect(
            "VBoxManage list systemproperties",
            Reply::ok(include_str!("../../tool_output/fixtures/vboxmanage/7.1.4-windows-systemproperties.txt")),
        )
        .build();
    let call = testing::call(&fake);
    assert_eq!(vms(&call).await.unwrap().len(), 3);
    assert_eq!(machine_folder(&call).await.unwrap(), std::path::PathBuf::from("C:\\Users\\ada\\VirtualBox VMs"));
}

#[tokio::test]
async fn a_state_with_none_printed_is_unexpected_never_absent() {
    let fake = FakeProcesses::script()
        .expect("VBoxManage showvminfo u1 --machinereadable", Reply::ok("name=\"x\""))
        .build();
    let e = vm_state(&testing::call(&fake), "u1").await.unwrap_err();
    assert!(matches!(e, CliError::Unexpected { .. }), "{e}");
}
