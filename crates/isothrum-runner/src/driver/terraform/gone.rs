//! A resource the run's state holds that its server no longer has (a reset server lost it, an
//! apply was cut short). The destroy reaches its provider's delete without a refresh, and a few
//! deletes fail on it rather than counting it done: the destroy would fail every time. The
//! manifest names those errors (`identity.terraform.gone`); a destroy that printed one, filled
//! with the attributes of an instance the state still holds, has that instance dropped from the
//! state (`terraform state rm`, through the destroy's own tool), then the `down` runs again
//! ([`crate::executor::run_down_steps`]). Nothing else is dropped: the text names its instance
//! by the instance's own attributes, in its provider's words.

use std::collections::BTreeSet;

use isoloom_manifest::{Argv, GoneError, HostArgv, HostOs, Run, Step, Template};
use serde_json::Value;

use super::state::array;
use crate::Result;
use crate::events::WarningCode;
use crate::executor::Cx;

/// One instance of the state, and what its delete prints when it is already gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Gone {
    /// Its address (`proxmox_virtual_environment_firewall_ipset.test`).
    pub(crate) address: String,
    /// Its error, filled (`no such IPSet 'iso1n0'`).
    pub(crate) text: String,
}

/// Each managed instance of `state` whose type `errors` names, with that error filled with its
/// attributes; an instance the error can't name (an attribute it lacks, or an empty one) is left
/// out.
pub(super) fn in_state(state: &Value, errors: &[GoneError]) -> Vec<Gone> {
    let mut out = Vec::new();
    for resource in array(state, "resources") {
        if resource.get("mode").and_then(Value::as_str) != Some("managed") {
            continue;
        }
        let kind = resource.get("type").and_then(Value::as_str);
        for error in errors.iter().filter(|e| Some(e.resource.as_str()) == kind) {
            for instance in array(resource, "instances") {
                let attributes = instance.get("attributes");
                let text = error.error.fill(|name| attributes.and_then(|a| a.get(name)).and_then(super::state::scalar));
                if let (Some(text), Some(address)) = (text, address(resource, instance)) {
                    out.push(Gone { address, text });
                }
            }
        }
    }
    out
}

/// An instance's address as Terraform writes it: its module's (`module.lab.`), its resource's
/// `<type>.<name>`, then its key (`[0]`, `["web"]`); none for a key of another kind.
fn address(resource: &Value, instance: &Value) -> Option<String> {
    let field = |key| resource.get(key).and_then(Value::as_str);
    let module = field("module").map(|m| format!("{m}.")).unwrap_or_default();
    let key = match instance.get("index_key") {
        None | Some(Value::Null) => String::new(),
        Some(n @ Value::Number(_)) => format!("[{n}]"),
        Some(s @ Value::String(_)) => format!("[{s}]"),
        Some(_) => return None,
    };
    Some(format!("{module}{}.{}{key}", field("type")?, field("name")?))
}

/// Drops from the state each of `held` (the instances it holds now) whose text is among `seen`,
/// what step `step` of list `list` printed before it failed, when that step is a Terraform
/// destroy: each named in a warning, then dropped through the destroy's own tool. Whether it
/// dropped any.
///
/// # Errors
///
/// `tool_failed` when `terraform state rm` fails.
pub(super) async fn forget(cx: &Cx<'_>, list: &str, step: &Step, held: Vec<Gone>, seen: &BTreeSet<String>) -> Result<bool> {
    let found: Vec<Gone> = held.into_iter().filter(|g| seen.contains(&g.text)).collect();
    let Some(rm) = state_rm(step, cx.run.runner.config.host, &found) else {
        return Ok(false);
    };
    for g in &found {
        let message = format!("`{}` is already gone ({}): dropped from the state, then destroyed again", g.address, g.text);
        cx.events.warning(WarningCode::AlreadyGone, &message);
    }
    crate::executor::run_argv(cx, list, &rm, &mut |_| {}).await?;
    Ok(true)
}

/// `step` as `<its tool> <its options before the subcommand> state rm <each address of gone>`,
/// its folder, environment and time limit kept (and its image, where the tool runs from one); none
/// when `gone` is empty or `step` isn't `terraform destroy` (or OpenTofu's) on `host`.
fn state_rm(step: &Step, host: HostOs, gone: &[Gone]) -> Option<Step> {
    let Run::Argv(command) = &step.run else { return None };
    let chosen = crate::executor::plan::for_host(command, host)?;
    let (at, subcommand) = crate::executor::plan::terraform_subcommand(chosen)?;
    if gone.is_empty() || subcommand != "destroy" {
        return None;
    }
    let mut args: Vec<Template> = chosen.args().iter().take(at).cloned().collect();
    args.extend(["state", "rm"].map(Template::text));
    args.extend(gone.iter().map(|g| Template::text(&g.address)));
    let mut rm = step.clone();
    rm.run = Run::Argv(HostArgv::Every(Argv::new(chosen.program().clone(), args)));
    rm.stdin = None;
    rm.verdict = None;
    Some(rm)
}

#[cfg(test)]
mod tests;
