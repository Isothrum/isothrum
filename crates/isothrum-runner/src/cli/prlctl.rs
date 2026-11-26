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
    super::stop_then_delete(call, TOOL, argv(["stop", uuid, "--kill"]), argv(["delete", uuid]), &NO_VM).await
}

#[cfg(test)]
mod tests;
