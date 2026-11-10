//! Isoloom's guest delivery over SSH (`guest` with `transport: ssh`): a machine's secrets and
//! inputs written into it by the runner, for a cloud with no secret store of its own
//! (DigitalOcean, Linode). The machine's set-up waits for `/var/lib/isoloom/secrets/.delivered`,
//! then puts each value where its spec says.
//!
//! - **When:** once the step named by the delivery's `during_step` succeeded, else once `up`'s
//!   steps all did (before the ready conditions, which wait for the set-up that waits for this).
//! - **Where:** the machine's address is the output `address_<machine>` (its `-` written `_`),
//!   its login the output `ssh_user_<machine>`, else `ssh_user`, else `root`; through the run's SSH
//!   jump.
//! - **How:** one SSH session per machine runs [`RECEIVE`] as root; its standard input holds one
//!   line per slot, `<slot name> <value in base64>`, from an owner-only file in the run's state
//!   folder removed right after (a removal that fails, fails the step), or as soon as the
//!   operation is cut short. A value is never on a command line nor in an event.
//! - **Retries:** while SSH can't connect (exit 255: a machine still booting), every
//!   [`RETRY`], for [`PATIENCE`]; any other failure fails the step.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use isoloom_manifest::{Delivery, SecretSlot};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call};
use crate::secrets::SecretValue;
use crate::ssh::{self, Login};

/// What runs as root on the machine: each `<name> <base64>` line of its input kept owner-only
/// under `/var/lib/isoloom/secrets/<name>` (a name of letters, digits, `_` and `-` only), then the
/// mark the set-up waits for.
pub(crate) const RECEIVE: &str = "set -e; umask 077; d=/var/lib/isoloom/secrets; mkdir -p \"$d\"; chmod 700 \"$d\"; \
while read -r n v; do case \"$n\" in ''|*[!A-Za-z0-9_-]*) echo \"isothrum: a secret's name is refused\" >&2; exit 2;; esac; \
printf %s \"$v\" | base64 -d > \"$d/$n.new\"; mv -f \"$d/$n.new\" \"$d/$n\"; done; touch \"$d/.delivered\"";

/// How long SSH may fail to connect before the delivery fails.
const PATIENCE: Duration = Duration::from_secs(900);
/// How long between two tries.
const RETRY: Duration = Duration::from_secs(5);
/// How long one try may go without progress (as every tool call's limit counts).
const TRY_LIMIT: Duration = Duration::from_secs(120);
/// SSH's own exit code when it couldn't connect (the command never ran).
const SSH_UNREACHABLE: i32 = 255;

/// Delivers to each machine the slots whose guest delivery waits for step `after` (`None`: for the
/// end of `up`'s steps).
pub(crate) async fn deliver(cx: &mut Cx<'_>, after: Option<&str>) -> Result<()> {
    let mut by_machine = due(cx, after);
    // A machine set up before this operation keeps what it was given (an update carries no
    // launch's values: delivering again would overwrite them).
    by_machine.retain(|machine, _| !cx.set_up.contains(machine));
    if by_machine.is_empty() {
        return Ok(());
    }
    let outputs = cx.run.read_outputs(cx).await?.values;
    // The login itself, though the module or the manifest keeps it secret.
    let output = |name: &str| outputs.iter().find(|o| o.name == name).and_then(|o| o.revealed().map(str::to_owned));
    for (machine, lines) in by_machine {
        let login = ssh::machine_login(output, &machine, cx.ssh_jump.as_deref())?.ok_or_else(|| Error::Malformed {
            at: format!("the guest delivery to `{machine}`"),
            reason: format!("no `{}` output gives the machine's address", ssh::address_output(&machine)),
        })?;
        to_machine(cx, &machine, &login, &lines).await?;
    }
    Ok(())
}

/// Each machine's input lines (`<slot> <base64>`) due after `after`.
fn due(cx: &Cx<'_>, after: Option<&str>) -> BTreeMap<String, String> {
    let given = |name: &str| cx.secrets.get(name).map(SecretValue::expose);
    due_lines(&cx.run.manifest().secrets, given, after)
}

/// [`due`] over `slots`, each slot's value from `given` else its default. A machine with a
/// delivery due gets its entry even when every value is unset (no lines): its set-up waits for
/// `.delivered`, which only a delivery writes.
fn due_lines<'a>(slots: &'a [SecretSlot], given: impl Fn(&str) -> Option<&'a str>, after: Option<&str>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for slot in slots {
        let value = given(slot.name.as_str()).or(slot.default.as_deref());
        for d in &slot.deliver {
            let Delivery::Guest { machine, during_step, .. } = d else {
                continue;
            };
            if during_step.as_ref().map(isoloom_manifest::StepId::as_str) != after {
                continue;
            }
            let lines = out.entry(machine.to_string()).or_default();
            let Some(v) = value else { continue };
            let encoded = crate::encoding::base64(v.as_bytes());
            lines.push_str(slot.name.as_str());
            lines.push(' ');
            lines.push_str(&encoded);
            lines.push('\n');
        }
    }
    out
}

/// One machine's delivery: `lines` piped into [`RECEIVE`] at `login`, retried while SSH can't
/// connect.
async fn to_machine(cx: &Cx<'_>, machine: &str, login: &Login, lines: &str) -> Result<()> {
    let id = format!("guest-{machine}");
    let note = format!("Delivering {machine}'s secrets…");
    cx.events
        .stepped(&id, Some(&note), async {
            let dir = cx.run.runner.store.run_dir(cx.run.name())?;
            let input = Input::write(dir.join(format!("{id}.in")), lines)?;
            let done = tries(cx, &id, login, input.path()).await;
            // The file holds the values: one left behind fails the delivery, unless it failed
            // already (that error stands; the next delivery writes the file again).
            let removed = input.remove();
            if let (Err(_), Err(e)) = (&done, &removed) {
                tracing::error!(error = %e, "a failed delivery's input file was left in the run's state folder");
            }
            done.and(removed)
        })
        .await
}

/// The owner-only file a delivery's values wait in, in the run's state folder. It goes when the
/// delivery ends, however it ends: [`Input::remove`] once delivered or failed, and on drop when
/// the operation is cut short mid-delivery (its future dropped).
struct Input {
    /// Where it is.
    path: PathBuf,
    /// Whether it is still to be removed.
    held: bool,
}

impl Input {
    /// `lines` written owner-only at `path`.
    fn write(path: PathBuf, lines: &str) -> Result<Input> {
        crate::store::write_private(&path, lines.as_bytes()).map_err(Error::io(&path, "written"))?;
        Ok(Input { path, held: true })
    }

    /// Where it is.
    fn path(&self) -> &Path {
        &self.path
    }

    /// Removes it; one that can't be removed is an error naming it.
    fn remove(mut self) -> Result<()> {
        self.held = false;
        std::fs::remove_file(&self.path).map_err(Error::io(&self.path, "removed"))
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if self.held
            && let Err(e) = std::fs::remove_file(&self.path)
        {
            tracing::error!(error = %e, path = %self.path.display(), "a cut-short delivery's input file was left in the run's state folder");
        }
    }
}

async fn tries(cx: &Cx<'_>, id: &str, login: &Login, input: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let mut spec = ssh::spec(cx, login, &format!("sudo sh -c {}", ssh::sh_quote(RECEIVE)), Vec::new())?;
        spec.stdin = Some(input.to_path_buf());
        let out = call(cx, id, &spec, TRY_LIMIT, Echo::Quiet).await?;
        match out.exit.code {
            Some(0) => return Ok(()),
            Some(SSH_UNREACHABLE) if tokio::time::Instant::now() < deadline => tokio::time::sleep(RETRY).await,
            _ => {
                return Err(Error::ToolFailed {
                    step: id.to_owned(),
                    reason: cx.events.mask(&out.reason()),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::due_lines;

    /// The solo-web cloud-vm manifest delivers `DEMO_SECRET` to `web`, over SSH, at the end of `up`.
    fn slots(default: Option<&str>) -> Vec<isoloom_manifest::SecretSlot> {
        let mut m = crate::testing::solo_web_aws_manifest();
        m.secrets
            .iter_mut()
            .filter(|s| s.name.as_str() == "DEMO_SECRET")
            .for_each(|s| s.default = default.map(str::to_owned));
        m.secrets
    }

    #[test]
    fn a_given_value_goes_to_its_machine_in_base64() {
        let due = due_lines(&slots(None), |n| (n == "DEMO_SECRET").then_some("token-x"), None);
        assert_eq!(due["web"], format!("DEMO_SECRET {}\n", crate::encoding::base64(b"token-x")));
    }

    #[test]
    fn a_machine_whose_values_are_all_unset_still_gets_its_delivery() {
        // No value and no default: no line, but the delivery still runs and writes `.delivered`.
        let due = due_lines(&slots(None), |_| None, None);
        assert_eq!(due.get("web").map(String::as_str), Some(""));
    }

    #[test]
    fn a_delivery_waits_for_its_step() {
        assert!(due_lines(&slots(Some("x")), |_| None, Some("some-step")).is_empty());
    }
}
