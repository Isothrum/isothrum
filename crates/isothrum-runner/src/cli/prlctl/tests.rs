use super::*;
use crate::cli::testing;
use crate::process::fake::{FakeProcesses, Reply};

#[tokio::test]
async fn a_vm_parallels_doesnt_know_is_removed_and_another_refusal_fails_with_what_it_said() {
    let gone = FakeProcesses::script()
        .expect(
            "prlctl stop u1 --kill",
            Reply::fail(255, "Failed to get VM config: The virtual machine could not be found."),
        )
        .expect("prlctl delete u1", Reply::fail(255, "Unable to find the VM by UUID u1"))
        .build();
    remove(&testing::call(&gone), "u1").await.expect("gone already");
    let refused = FakeProcesses::script()
        .expect("prlctl stop u1 --kill", Reply::ok(""))
        .expect("prlctl delete u1", Reply::fail(255, "Failed to delete the VM: permission denied"))
        .build();
    let e = remove(&testing::call(&refused), "u1").await.expect_err("refused");
    assert!(
        matches!(&e, CliError::Failed { tool: "prlctl", failure } if failure.starts_with("Failed to delete the VM: permission denied")),
        "{e:?}"
    );
}

#[tokio::test]
async fn a_listing_without_the_vms_home_is_unexpected_never_a_bundle_guessed() {
    let fake = FakeProcesses::script()
        .expect("prlctl list --info --json u1", Reply::ok(r#"[{"ID": "{u1}"}]"#))
        .build();
    let e = home(&testing::call(&fake), "u1").await.expect_err("no Home");
    assert!(matches!(e, CliError::Unexpected { tool: "prlctl", .. }), "{e:?}");
}
