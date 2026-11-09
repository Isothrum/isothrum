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
//!   folder removed right after. A value is never on a command line nor in an event.
//! - **Retries:** while SSH can't connect (exit 255: a machine still booting), every
//!   [`RETRY`], for [`PATIENCE`]; any other failure fails the step.

use std::collections::BTreeMap;
use std::time::Duration;

use isoloom_manifest::{Delivery, SecretSlot};

use crate::Result;
use crate::error::Error;
use crate::events::StepState;
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
/// How long one try may take.
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
    let outputs = cx.run.read_outputs(cx).await?;
    let output = |name: &str| outputs.iter().find(|o| o.name == name).and_then(|o| o.value.clone());
    for (machine, lines) in by_machine {
        let key = machine.replace('-', "_");
        let host = output(&format!("address_{key}")).ok_or_else(|| Error::Unsupported {
            what: format!("a guest delivery to `{machine}` without its `address_{key}` output"),
        })?;
        let user = ssh::machine_user(output, &key);
        to_machine(cx, &machine, &Login::new(&user, &host, cx.ssh_jump.as_deref(), "")?, &lines).await?;
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
    cx.events.step(&id, StepState::Running, Some(&format!("Delivering {machine}'s secrets…")));
    let dir = cx.run.runner.store.run_dir(cx.run.name())?;
    let input = dir.join(format!("{id}.in"));
    crate::store::write_private(&input, lines.as_bytes()).map_err(Error::io(&input, "written"))?;
    let done = tries(cx, &id, login, &input).await;
    if let Err(e) = std::fs::remove_file(&input) {
        tracing::warn!(error = %e, "a delivery's input file was left in the run's state folder");
    }
    cx.events.step(&id, if done.is_ok() { StepState::Done } else { StepState::Failed }, None);
    done
}

async fn tries(cx: &Cx<'_>, id: &str, login: &Login, input: &std::path::Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + PATIENCE;
    loop {
        let mut spec = ssh::spec(cx, login, &format!("sudo sh -c {}", ssh::sh_quote(RECEIVE)), Vec::new())?;
        spec.stdin = Some(input.to_path_buf());
        let out = call(cx, id, &spec, TRY_LIMIT, Echo::Quiet).await?;
        match out.exit.and_then(|e| e.code) {
            Some(0) => return Ok(()),
            Some(SSH_UNREACHABLE) if tokio::time::Instant::now() < deadline => tokio::time::sleep(RETRY).await,
            _ => {
                return Err(Error::ToolFailed {
                    step: id.to_owned(),
                    reason: cx.events.mask(&crate::executor::why_failed(&out)),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::due_lines;

    /// The solo-web cloud-vm manifest delivers `FLAG` to `web`, over SSH, at the end of `up`.
    fn slots(default: Option<&str>) -> Vec<isoloom_manifest::SecretSlot> {
        let mut m = crate::testing::solo_web_aws_manifest();
        m.secrets
            .iter_mut()
            .filter(|s| s.name.as_str() == "FLAG")
            .for_each(|s| s.default = default.map(str::to_owned));
        m.secrets
    }

    #[test]
    fn a_given_value_goes_to_its_machine_in_base64() {
        let due = due_lines(&slots(None), |n| (n == "FLAG").then_some("CTF{x}"), None);
        assert_eq!(due["web"], format!("FLAG {}\n", crate::encoding::base64(b"CTF{x}")));
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
