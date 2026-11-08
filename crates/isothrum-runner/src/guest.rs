//! Isoloom's guest delivery over SSH (`guest` with `transport: ssh`): a machine's secrets and
//! inputs written into it by the runner, for a cloud with no secret store of its own
//! (DigitalOcean, Linode). The machine's set-up waits for `/var/lib/isoloom/secrets/.delivered`,
//! then puts each value where its spec says.
//!
//! - **When:** once the step named by the delivery's `during_step` succeeded, else once `up`'s
//!   steps all did (before the ready conditions, which wait for the set-up that waits for this).
//! - **Where:** the machine's address is the output `address_<machine>` (its `-` written `_`),
//!   its login the output `ssh_user_<machine>`, else `ssh_user`; through the run's SSH jump.
//! - **How:** one SSH session per machine runs [`RECEIVE`] as root; its standard input holds one
//!   line per slot, `<slot name> <value in base64>`, from an owner-only file in the run's state
//!   folder removed right after. A value is never on a command line nor in an event.
//! - **Retries:** while SSH can't connect (exit 255: a machine still booting), every
//!   [`RETRY`], for [`PATIENCE`]; any other failure fails the step.

use std::collections::BTreeMap;
use std::time::Duration;

use isoloom_manifest::{Delivery, Transport};

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
    let by_machine = due(cx, after)?;
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
        let user = output(&format!("ssh_user_{key}"))
            .or_else(|| output("ssh_user"))
            .unwrap_or_else(|| "root".into());
        to_machine(cx, &machine, &Login::new(&user, &host, cx.ssh_jump.as_deref(), "")?, &lines).await?;
    }
    Ok(())
}

/// Each machine's input lines (`<slot> <base64>`) due after `after`; refused for a transport the
/// runner doesn't deliver over.
fn due(cx: &Cx<'_>, after: Option<&str>) -> Result<BTreeMap<String, String>> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for slot in &cx.run.manifest().secrets {
        let value = cx.secrets.get(slot.name.as_str()).map(SecretValue::expose).or(slot.default.as_deref());
        for d in &slot.deliver {
            let Delivery::Guest {
                machine,
                transport,
                during_step,
            } = d
            else {
                continue;
            };
            if during_step.as_ref().map(isoloom_manifest::StepId::as_str) != after {
                continue;
            }
            if *transport != Transport::Ssh {
                return Err(Error::Unsupported {
                    what: format!("a guest delivery to `{machine}` over {transport:?}"),
                });
            }
            let Some(v) = value else { continue };
            let encoded = crate::encoding::base64(v.as_bytes());
            let lines = out.entry(machine.to_string()).or_default();
            lines.push_str(slot.name.as_str());
            lines.push(' ');
            lines.push_str(&encoded);
            lines.push('\n');
        }
    }
    Ok(out)
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
                    reason: cx.events.mask(&reason(&out.stderr, &out.stdout)),
                });
            }
        }
    }
}

/// Why a try failed: its standard error, else its standard output (a cloud image refusing the
/// login prints why there: "Please login as the user \"admin\""), else a plain note.
fn reason(stderr: &[String], stdout: &[String]) -> String {
    let text = |lines: &[String]| lines.join("\n").trim().to_owned();
    [text(stderr), text(stdout)]
        .into_iter()
        .find(|t| !t.is_empty())
        .unwrap_or_else(|| "the command failed with no output".to_owned())
}

#[cfg(test)]
mod tests {
    use super::reason;

    #[test]
    fn a_failure_says_why_from_stderr_then_stdout() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(reason(&lines(&["denied"]), &lines(&["noise"])), "denied");
        assert_eq!(
            reason(&lines(&["", " "]), &lines(&["Please login as the user \"admin\""])),
            "Please login as the user \"admin\""
        );
        assert_eq!(reason(&[], &[]), "the command failed with no output");
    }
}
