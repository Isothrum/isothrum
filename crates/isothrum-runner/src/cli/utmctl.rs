//! The UTM adapter: a VM's name, which UTM's links take, from `utmctl list`.

use super::{Call, CliError, argv};
use crate::process::Captured;

/// The tool.
const TOOL: &str = "utmctl";

/// The name `utmctl list` gives VM `uuid` (`<uuid>  <status>  <name>` per line); none when it
/// lists no such VM.
pub(crate) async fn name_of(call: &Call<'_>, uuid: &str) -> Result<Option<String>, CliError<String>> {
    let out = call.ok(TOOL, argv(["list"]), Captured::reason).await?;
    Ok(named(&out.stdout, uuid))
}

/// The name of VM `uuid` among `lines`.
fn named(lines: &[String], uuid: &str) -> Option<String> {
    lines.iter().find_map(|line| {
        let rest = line.trim_start().strip_prefix(uuid)?;
        let (_status, name) = rest.trim_start().split_once(char::is_whitespace)?;
        Some(name.trim().to_owned())
    })
}

/// What `utmctl` says of a VM it doesn't know.
const NO_VM: [&str; 2] = ["virtual machine not found", "no virtual machine"];

/// Removes VM `uuid` by its id: `stop --kill` (one already stopped is fine), then `delete` (its
/// bundle goes). A VM UTM doesn't know is gone already.
pub(crate) async fn remove(call: &Call<'_>, uuid: &str) -> Result<(), CliError<String>> {
    drop(call.run::<String>(TOOL, argv(["stop", "--kill", uuid])).await?);
    let out = call.run::<String>(TOOL, argv(["delete", uuid])).await?;
    super::done_or_gone(TOOL, &out, &NO_VM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vm_is_named_by_its_line() {
        let lines: Vec<String> = [
            "UUID                                 Status   Name",
            "AB-12  started  web vm",
            "CD-34  stopped  db",
        ]
        .map(str::to_owned)
        .to_vec();
        assert_eq!(named(&lines, "AB-12").as_deref(), Some("web vm"));
        assert_eq!(named(&lines, "EF-56"), None);
    }
}
