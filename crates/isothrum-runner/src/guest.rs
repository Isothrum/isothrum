//! Isoloom's guest delivery over SSH (`guest` with `transport: ssh`): a machine's secrets and
//! inputs written into it by the runner, for a cloud with no secret store of its own
//! (DigitalOcean, Linode). The machine's set-up waits for `/var/lib/isoloom/secrets/.delivered`,
//! then puts each value where its spec says.
//!
//! - **When:** once the step named by the delivery's `during_step` succeeded, else once `up`'s
//!   steps all did (before the ready conditions, which wait for the set-up that waits for this).
//! - **Where:** the machine's address is the output `address_<machine>`, its login the output
//!   `ssh_user_<machine>`, else `ssh_user`, else `root` (the machine's name as Isoloom writes it
//!   there: `-` and `.` as `_`, in lower case); through the run's SSH jump.
//! - **How:** one SSH session per machine runs [`receive`] as root; its standard input holds one
//!   line per slot, `<slot name> <value in base64>`, from an owner-only file in the run's state
//!   folder removed right after (a removal that fails, fails the step), or as soon as the
//!   operation is cut short. A value is never on a command line nor in an event.
//! - **Retries:** while SSH can't connect (exit 255, nothing printed: a machine still booting),
//!   every [`ssh::RETRY`], for [`PATIENCE`] ([`ssh::Retrying`]), then the step fails with
//!   `unreachable`; any other failure fails the step at once.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use isoloom_manifest::{Delivery, SecretSlot};

use crate::Result;
use crate::error::Error;
use crate::executor::{Cx, Echo, call, slot_value, stdout_of};
use crate::ssh::{self, Login};
use crate::store::Transient;
use isoloom_manifest::contract::{DELIVERED, SECRETS_DIR};

/// What runs as root on the machine: each `<name> <base64>` line of its input kept owner-only
/// under [`SECRETS_DIR`]`/<name>` (a name of letters, digits, `_` and `-` only), then the mark the
/// set-up waits for ([`DELIVERED`]).
fn receive() -> String {
    format!(
        "set -e; umask 077; d={SECRETS_DIR}; mkdir -p \"$d\"; chmod 700 \"$d\"; \
while read -r n v; do case \"$n\" in ''|*[!A-Za-z0-9_-]*) echo \"isothrum: a secret's name is refused\" >&2; exit 2;; esac; \
printf %s \"$v\" | base64 -d > \"$d/$n.new\"; mv -f \"$d/$n.new\" \"$d/$n\"; done; touch {DELIVERED}"
    )
}

/// How long SSH may fail to connect before the delivery fails.
const PATIENCE: Duration = Duration::from_secs(900);
/// How long one try may go without progress (as every tool call's limit counts).
const TRY_LIMIT: Duration = Duration::from_secs(120);

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

/// Each machine's input lines (`<slot> <base64>`) due after `after`, each slot's value as every
/// delivery has it ([`slot_value`]: given, else its default; none for another provider's slot).
fn due(cx: &Cx<'_>, after: Option<&str>) -> BTreeMap<String, String> {
    due_lines(&cx.manifest().secrets, |slot| slot_value(cx, slot), after)
}

/// [`due`] over `slots`, each slot's value from `value`. A machine with a delivery due gets its
/// entry even when every value is unset (no lines): its set-up waits for `.delivered`, which only
/// a delivery writes.
fn due_lines<'a>(slots: &'a [SecretSlot], value: impl Fn(&'a SecretSlot) -> Option<&'a str>, after: Option<&str>) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for slot in slots {
        let value = value(slot);
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

/// One machine's delivery: `lines` piped into [`receive`] at `login`, retried while SSH can't
/// connect.
async fn to_machine(cx: &Cx<'_>, machine: &str, login: &Login, lines: &str) -> Result<()> {
    let id = format!("guest-{machine}");
    let note = format!("Delivering {machine}'s secrets…");
    cx.events
        .stepped(&id, Some(&note), async {
            // The file holds the values: it goes however the delivery ends, and one left behind
            // fails the delivery, unless it failed already (that error stands).
            let input = Transient::write(cx.state_dir().join(format!("{id}.in")), lines.as_bytes(), "a guest delivery's input file")?;
            let done = deliver_retrying(cx, &id, login, input.path()).await;
            input.remove_after(done)
        })
        .await
}

/// Pipes `input` into [`receive`], run as root at `login` for the step `id`: tried again while SSH
/// can't connect, for [`PATIENCE`] ([`ssh::Retrying`], as an `ssh.run` step's); SSH still
/// not connecting then fails the step with `unreachable`, any other failure with why.
async fn deliver_retrying(cx: &Cx<'_>, id: &str, login: &Login, input: &Path) -> Result<()> {
    let mut spec = ssh::spec(cx, login, &format!("sudo sh -c {}", crate::console::sh_quote(&receive())), Vec::new())?;
    spec.stdin = Some(input.to_path_buf());
    let wait = ssh::Wait {
        limit: PATIENCE,
        login: login.target.clone(),
    };
    let mut retrying = ssh::Retrying::new(&wait);
    let out = loop {
        let out = call(cx, id, &spec, TRY_LIMIT, Echo::Quiet).await?;
        if !retrying.again(ssh::Try::unreachable(&out), || {}).await {
            break out;
        }
    };
    let out = retrying.answered(cx, id, out)?;
    stdout_of(cx, id, None, &out).map(drop)
}

#[cfg(test)]
mod tests {
    use super::due_lines;

    /// The solo-web cloud-vm manifest delivers `DEMO_SECRET` to `web`, over SSH, at the end of `up`.
    fn slots() -> Vec<isoloom_manifest::SecretSlot> {
        crate::testing::solo_web_aws_manifest().secrets
    }

    #[test]
    fn a_given_value_goes_to_its_machine_in_base64() {
        let due = due_lines(&slots(), |s| (s.name.as_str() == "DEMO_SECRET").then_some("token-x"), None);
        assert_eq!(due["web"], format!("DEMO_SECRET {}\n", crate::encoding::base64(b"token-x")));
    }

    #[test]
    fn a_machine_whose_values_are_all_unset_still_gets_its_delivery() {
        // No value: no line, but the delivery still runs and writes `.delivered`.
        let due = due_lines(&slots(), |_| None, None);
        assert_eq!(due.get("web").map(String::as_str), Some(""));
    }

    #[test]
    fn a_delivery_waits_for_its_step() {
        assert!(due_lines(&slots(), |_| Some("x"), Some("some-step")).is_empty());
    }
}
