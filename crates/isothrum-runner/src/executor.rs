//! The manifest executor: a list of steps in order, each an argv or a driver action, under its
//! `when`, `failure`, host and provider filters, with its timeout, environment and deliveries.
//! Everything generic lives here, the actions every driver shares among it (`ssh.run`, a command
//! in a guest; `project.archive`, the project for a guest); a driver implements only its own
//! actions and how it reads status.

mod delivery;
pub(crate) mod plan;
mod reconnect;
pub(crate) mod state_files;
mod step;

pub(crate) use delivery::{env_deliveries, every_delivery, step_env};

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use isoloom_manifest::{Action, Failure, Run, Step, When};
use tracing::Instrument;

pub(crate) use step::{Echo, call, call_ok, capture_step, env_of, run_argv, spec_for, stdin_of, why_failed};

use crate::Result;
use crate::error::Error;
use crate::events::WarningCode;
use crate::events::{Emitter, StepState};
use crate::run::Running;
use crate::scope::Scope;
use crate::secrets::Secrets;
use crate::vars::Vars;

/// What one operation's steps run with.
pub(crate) struct Cx<'a> {
    /// The run.
    pub(crate) run: &'a Running<'a>,
    /// Its placeholders' values.
    pub(crate) vars: Vars,
    /// The secret and input values given to this operation.
    pub(crate) secrets: &'a Secrets,
    /// Its events.
    pub(crate) events: &'a Emitter<'a>,
    /// This host, and the run's Vagrant provider when it has one.
    pub(crate) scope: Scope,
    /// Starts the tools: the runner's, or an elevated session's for an operation on Hyper-V.
    pub(crate) processes: std::sync::Arc<dyn crate::process::ProcessRunner>,
    /// What the steps' lines say of leftovers from an earlier run.
    pub(crate) stale: Stale,
    /// The login every SSH of the run goes through first, when it has one.
    pub(crate) ssh_jump: Option<String>,
    /// When the run stops itself (seconds since the Unix epoch), for a manifest with an `expiry`.
    pub(crate) expires_at: Option<u64>,
    /// The macOS relays this operation's tools go through, when its run needs them.
    pub(crate) relayed: Option<crate::relay::Relayed>,
}

/// The manifest's `recovery.stale_markers`, watched over every line the steps print, whatever its
/// case (Vagrant prints VirtualBox's `VERR_ALREADY_EXISTS` in capitals, the manifest writes it in
/// lower case; and the line may be any of the step's, not one of the last it printed).
#[derive(Debug, Default)]
pub(crate) struct Stale {
    markers: Vec<String>,
    seen: Mutex<Option<String>>,
}

impl Stale {
    /// A watch over `markers`.
    pub(crate) fn new(markers: &[String]) -> Stale {
        Stale {
            markers: markers.iter().map(|m| m.to_lowercase()).filter(|m| !m.is_empty()).collect(),
            seen: Mutex::new(None),
        }
    }

    /// Notes `line` when it holds a marker.
    fn notice(&self, line: &str) {
        let lower = line.to_lowercase();
        if let Some(m) = self.markers.iter().find(|m| lower.contains(m.as_str())) {
            *self.seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(m.clone());
        }
    }

    /// The marker seen since the last call, if any.
    pub(crate) fn take(&self) -> Option<String> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// The steps of a list that ran and succeeded, or that don't apply on this host or provider.
#[derive(Debug, Default)]
pub(crate) struct Ran {
    pub(crate) ok: BTreeSet<String>,
}

/// The start's list, as messages name it.
pub(crate) const UP: &str = "lifecycle.up";

/// Runs `steps` (the manifest's list `list`, for messages) in order. After a failure, `if_ok`
/// steps are skipped and `always` steps still run; a `tolerate` step's failure is a warning.
/// The first failure is the list's. A step naming an output or a machine's host that has no
/// value yet reads the driver's state first (a guest step to `${outputs.ip}` right after the
/// `apply` that made the VM).
pub(crate) async fn run_steps(cx: &mut Cx<'_>, list: &str, steps: &[Step]) -> Result<Ran> {
    let mut ran = Ran::default();
    let mut failure: Option<Error> = None;
    for step in steps {
        let id = step.id.as_str();
        if !plan::applies(&cx.scope, step) {
            ran.ok.insert(id.to_owned());
            continue;
        }
        if failure.is_some() && step.when == When::IfOk {
            cx.events.step(id, StepState::Skipped, None);
            continue;
        }
        let span = tracing::info_span!("step", list, step = id);
        let done = match run_step(cx, list, step).instrument(span).await {
            // A guest delivery waiting for this start step goes once it succeeded.
            Ok(()) if list == UP => crate::guest::deliver(cx, Some(id)).await,
            other => other,
        };
        match done {
            Ok(()) => {
                ran.ok.insert(id.to_owned());
            }
            Err(e) if step.failure == Failure::Tolerate => {
                cx.events.warning(WarningCode::StepTolerated, &e.to_string());
            }
            Err(e) => {
                failure.get_or_insert(e);
            }
        }
    }
    failure.map_or(Ok(ran), Err)
}

/// Runs one step, with its `step` events: the driver's state read first when the step names
/// what only it gives.
async fn run_step(cx: &mut Cx<'_>, list: &str, step: &Step) -> Result<()> {
    let id = step.id.as_str();
    cx.events.step(id, StepState::Running, step.note.as_deref());
    let mut done = Ok(());
    if awaits_state(&cx.vars, step) {
        let run = cx.run;
        done = match run.observe(cx).await {
            Ok(observed) => {
                run.bind_observed(cx, observed);
                Ok(())
            }
            Err(e) => Err(e),
        };
    }
    if done.is_ok() {
        let cx: &Cx<'_> = cx;
        done = match &step.run {
            // `ssh.run` is a command too: its lines are the step's (a check runner's results).
            Run::Argv(_) | Run::Action { action: Action::SshRun, .. } => run_argv(cx, list, step, &mut |line| cx.stale.notice(line)).await,
            // The project's archive is every driver's: the runner's own.
            Run::Action {
                action: Action::ProjectArchive,
                with,
            } => crate::archive::project(cx, step, with).await,
            Run::Action { action, with } => cx.run.driver.action(cx, step, *action, with).await,
        };
    }
    match &done {
        Ok(()) => tracing::debug!("step done"),
        Err(e) => tracing::debug!(error = %e, "step failed"),
    }
    cx.events.step(id, if done.is_ok() { StepState::Done } else { StepState::Failed }, None);
    done
}

/// Whether `step` names an output or a machine's host that `vars` has no value for yet.
fn awaits_state(vars: &Vars, step: &Step) -> bool {
    let argvs: Vec<&isoloom_manifest::Argv> = match &step.run {
        Run::Argv(isoloom_manifest::HostArgv::Every(a)) => vec![a],
        Run::Argv(isoloom_manifest::HostArgv::PerHost(p)) => [&p.default, &p.linux, &p.macos, &p.windows].into_iter().flatten().collect(),
        Run::Action { .. } => Vec::new(),
    };
    let with = match &step.run {
        Run::Action { with, .. } => with.values().collect(),
        Run::Argv(_) => Vec::new(),
    };
    argvs
        .into_iter()
        .flat_map(|a| a.args().iter())
        .chain(with)
        .chain(step.env.values())
        .chain(step.stdin.iter().map(isoloom_manifest::PathTemplate::template))
        .flat_map(isoloom_manifest::Template::placeholders)
        .any(|p| vars.awaits_state(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_naming_an_output_or_a_host_without_a_value_awaits_the_drivers_state() {
        let step = |json: &str| serde_json::from_str::<Step>(json).unwrap();
        let ssh = step(r#"{"id":"copy","action":"ssh.run","with":{"host":"${outputs.ip}","command":"true"}}"#);
        let piped = step(r#"{"id":"open","argv":["curl","http://${host.web}/"]}"#);
        let plain = step(r#"{"id":"apply","argv":["terraform","apply"],"env":{"TF_DATA_DIR":"${run.state_dir}/.terraform"}}"#);
        let mut vars = Vars::default();
        assert!(awaits_state(&vars, &ssh) && awaits_state(&vars, &piped));
        assert!(!awaits_state(&vars, &plain));
        vars.set_output("ip", "10.0.0.7".into());
        vars.set_hosts(std::collections::BTreeMap::from([("web".to_owned(), "127.0.0.1".to_owned())]));
        assert!(!awaits_state(&vars, &ssh) && !awaits_state(&vars, &piped));
    }

    #[test]
    fn a_stale_marker_is_seen_in_any_line_whatever_its_case() {
        // Regression (R8): the launcher matched lower-case markers against VirtualBox's
        // `VERR_ALREADY_EXISTS`, and only in the error's last lines.
        let stale = Stale::new(&["verr_already_exists".into(), "is locked".into()]);
        stale.notice("    web: Progress state: VBOX_E_FILE_ERROR");
        assert_eq!(stale.take(), None);
        stale.notice("VBoxManage: error: Details: code VERR_ALREADY_EXISTS (0x80004005)");
        stale.notice("ok");
        assert_eq!(stale.take().as_deref(), Some("verr_already_exists"));
        assert_eq!(stale.take(), None, "taken once");
        let none = Stale::new(&[]);
        none.notice("anything");
        assert_eq!(none.take(), None);
    }
}
