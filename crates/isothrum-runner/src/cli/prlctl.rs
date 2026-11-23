//! The Parallels adapter: where `prlctl` says a VM's bundle is.

use super::{Call, CliError, argv};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "prlctl";

/// The bundle folder of VM `uuid` (`prlctl list --info --json <uuid>`'s `Home`).
pub(crate) async fn home(call: &Call<'_>, uuid: &str) -> Result<String, CliError<String>> {
    let out = call.ok(TOOL, argv(["list", "--info", "--json", uuid]), Captured::reason).await?;
    crate::tool_output::prlctl::home(&out.stdout.join("\n")).ok_or_else(|| CliError::Unexpected {
        tool: TOOL,
        what: format!("a `list --info` of `{uuid}` with no Home"),
    })
}

/// What `prlctl` says of a VM it doesn't know.
const NO_VM: [&str; 2] = ["unable to find the vm", "failed to get vm config"];

/// Removes VM `uuid` by its id: `stop --kill` (one already stopped is fine), then `delete` (its
/// bundle goes). A VM Parallels doesn't know is gone already.
pub(crate) async fn remove(call: &Call<'_>, uuid: &str) -> Result<(), CliError<String>> {
    drop(call.run::<String>(TOOL, argv(["stop", uuid, "--kill"])).await?);
    let out = call.run::<String>(TOOL, argv(["delete", uuid])).await?;
    super::done_or_gone(TOOL, &out, &NO_VM)
}

#[cfg(test)]
mod tests {
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
}
