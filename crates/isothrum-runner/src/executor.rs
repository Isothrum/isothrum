//! The manifest executor: a list of steps in order, each an argv or a driver action, under its
//! `when`, `failure`, host and provider filters, with its timeout, environment and deliveries.
//! Everything generic lives here, the actions every driver shares among it (`ssh.run`, a command
//! in a guest; `project.archive`, the project for a guest); a driver implements only its own
//! actions and how it reads status.

mod delivery;
pub(crate) mod plan;
mod progress;
mod reconnect;
pub(crate) mod state_files;
mod step;

pub(crate) use delivery::{env_deliveries, every_delivery};

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use isoloom_manifest::{Action, Failure, Run, Step, When};
use tracing::Instrument;

pub(crate) use progress::{Progress, within};
pub(crate) use step::{Echo, call, call_ok, capture_output, capture_step, env_of, run_argv, stdin_of, stdout_of, with_delivered};

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
    /// This host, and the run's provider when it has one (whatever its driver).
    pub(crate) scope: Scope,
    /// Starts the tools: the runner's, or an elevated session's for an operation on Hyper-V.
    pub(crate) processes: std::sync::Arc<dyn crate::process::ProcessRunner>,
    /// What the steps' lines say of leftovers from an earlier run.
    pub(crate) stale: LineWatch,
    /// What the steps' lines say of resources already gone, while a `down` watches for them.
    pub(crate) gone: LineWatch,
    /// The login every SSH of the run goes through first, when it has one.
    pub(crate) ssh_jump: Option<String>,
    /// When the run stops itself (seconds since the Unix epoch), for a manifest with an `expiry`.
    pub(crate) expires_at: Option<u64>,
    /// The macOS relays this operation's tools go through, when its run needs them.
    pub(crate) relayed: Option<crate::relay::Relayed>,
    /// The machines that were set up before this operation (an update's running machines): they
    /// keep what they were given, so no guest delivery goes to them again.
    pub(crate) set_up: std::collections::BTreeSet<String>,
}

/// Texts watched for in every line an argv or `ssh.run` step prints (a driver action's own calls
/// aren't watched): any line of the step's, not only one of the last it printed. Two watch the
/// steps: the manifest's `recovery.stale_markers`, whatever their case
/// ([`LineWatch::any_case`]: Vagrant prints VirtualBox's `VERR_ALREADY_EXISTS` in capitals, the
/// manifest writes it in lower case), and, exactly as written, what a destroy prints for a
/// resource already gone ([`run_down_steps`]).
#[derive(Debug, Default)]
pub(crate) struct LineWatch {
    /// Whether a text is seen whatever its case (the texts are then kept in lower case).
    any_case: bool,
    texts: Mutex<BTreeSet<String>>,
    seen: Mutex<BTreeSet<String>>,
}

impl LineWatch {
    /// A watch over `texts`, seen whatever their case.
    pub(crate) fn any_case(texts: &[String]) -> LineWatch {
        let watch = LineWatch {
            any_case: true,
            ..LineWatch::default()
        };
        watch.watch(texts.iter().cloned());
        watch
    }

    /// Watches for `texts` from now on, in place of what it watched for, and forgets what it saw.
    pub(crate) fn watch(&self, texts: impl IntoIterator<Item = String>) {
        let texts = texts.into_iter().filter(|t| !t.is_empty());
        *self.texts.lock().unwrap_or_else(PoisonError::into_inner) = if self.any_case {
            texts.map(|t| t.to_lowercase()).collect()
        } else {
            texts.collect()
        };
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }

    /// Notes each text `line` holds.
    fn notice(&self, line: &str) {
        let texts = self.texts.lock().unwrap_or_else(PoisonError::into_inner);
        if texts.is_empty() {
            return;
        }
        let line = if self.any_case { line.to_lowercase() } else { line.to_owned() };
        let found = texts.iter().filter(|t| line.contains(t.as_str())).cloned();
        self.seen.lock().unwrap_or_else(PoisonError::into_inner).extend(found);
    }

    /// The texts seen since the last take, or since it was told what to watch for.
    pub(crate) fn take(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.seen.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// [`LineWatch::take`], and nothing watched any more.
    pub(crate) fn end(&self) -> BTreeSet<String> {
        self.texts.lock().unwrap_or_else(PoisonError::into_inner).clear();
        self.take()
    }
}

/// The steps of a list that ran and succeeded, or that don't apply on this host or provider.
#[derive(Debug, Default)]
pub(crate) struct Ran {
    pub(crate) ok: BTreeSet<String>,
}

/// The start's list, as messages name it.
pub(crate) const UP: &str = "lifecycle.up";

/// The removal's list, as messages name it.
pub(crate) const DOWN: &str = "lifecycle.down";

/// How many times a `down` drops resources already gone from the run's state and runs again.
const GONE_ROUNDS: u8 = 3;

/// Runs `steps` (the manifest's list `list`, for messages) in order. After a failure, `if_ok`
/// steps are skipped and `always` steps still run; a `tolerate` step's failure is a warning.
/// The first failure is the list's. A step naming an output or a machine's host that has no
/// value yet reads the driver's state first (a guest step to `${outputs.ip}` right after the
/// `apply` that made the VM).
pub(crate) async fn run_steps(cx: &mut Cx<'_>, list: &str, steps: &[Step]) -> Result<Ran> {
    run_list(cx, list, steps, AfterStep::Nothing).await
}

/// The manifest's start steps (`lifecycle.up`), as [`run_steps`] runs a list, each guest
/// delivery waiting for a step going once that step succeeded.
pub(crate) async fn run_up_steps(cx: &mut Cx<'_>, steps: &[Step]) -> Result<Ran> {
    run_list(cx, UP, steps, AfterStep::DeliverToGuests).await
}

/// The manifest's removal steps (`lifecycle.down`), as [`run_steps`] runs a list. A step that
/// failed after printing what its driver says of a resource the run's state still holds that is
/// already gone (a Terraform destroy, the provider's error the manifest's `identity.terraform.gone`
/// names, filled for that resource) has those resources dropped from the state, each named in a
/// warning, then the steps run again, up to [`GONE_ROUNDS`] times. Any other failure is the
/// list's, and nothing that might still exist is dropped.
pub(crate) async fn run_down_steps(cx: &mut Cx<'_>) -> Result<Ran> {
    let down = cx.run.manifest().lifecycle.down.as_slice();
    for _ in 0..GONE_ROUNDS {
        cx.gone.watch(cx.run.driver.gone(cx)?.into_iter().map(|g| g.text));
        let done = run_steps(cx, DOWN, down).await;
        let seen = cx.gone.end();
        let failed = match &done {
            Err(Error::ToolFailed { step, .. }) if !seen.is_empty() => down.iter().find(|s| s.id.as_str() == step),
            _ => None,
        };
        let Some(step) = failed else { return done };
        if !cx.run.driver.forget_gone(cx, DOWN, step, &seen).await? {
            return done;
        }
    }
    run_steps(cx, DOWN, down).await
}

/// What follows each step of a list that succeeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AfterStep {
    /// Nothing.
    Nothing,
    /// The guest deliveries that wait for that step ([`crate::guest::deliver`]).
    DeliverToGuests,
}

/// [`run_steps`], with `after` each step that succeeded.
async fn run_list(cx: &mut Cx<'_>, list: &str, steps: &[Step], after: AfterStep) -> Result<Ran> {
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
            Ok(()) if after == AfterStep::DeliverToGuests => crate::guest::deliver(cx, Some(id)).await,
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
    let events = cx.events;
    let done = events.stepped(step.id.as_str(), step.note.as_deref(), step_work(cx, list, step)).await;
    match &done {
        Ok(()) => tracing::debug!("step done"),
        Err(e) => tracing::debug!(error = %e, "step failed"),
    }
    done
}

/// What one step does, the driver's state read first when it names what only that gives.
async fn step_work(cx: &mut Cx<'_>, list: &str, step: &Step) -> Result<()> {
    if awaits_state(&cx.vars, step) {
        let run = cx.run;
        let observed = run.observe(cx).await?;
        run.bind_observed(cx, observed);
    }
    let cx: &Cx<'_> = cx;
    match &step.run {
        // `ssh.run` is a command too: its lines are the step's (a check runner's results).
        Run::Argv(_) | Run::Action { action: Action::SshRun, .. } => {
            run_argv(cx, list, step, &mut |line| {
                cx.stale.notice(line);
                cx.gone.notice(line);
            })
            .await
        }
        // The project's archive is every driver's: the runner's own.
        Run::Action {
            action: Action::ProjectArchive,
            with,
        } => crate::archive::project(cx, list, step, with).await,
        Run::Action { action, .. } => cx.run.driver.action(cx, list, step, *action).await,
    }
}

/// Whether `step` names an output or a machine's host that `vars` has no value for yet.
fn awaits_state(vars: &Vars, step: &Step) -> bool {
    let argvs: Vec<&isoloom_manifest::Argv> = match &step.run {
        Run::Argv(argv) => plan::every_argv(argv).collect(),
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
        // Regression: lower-case markers matched against VirtualBox's `VERR_ALREADY_EXISTS`,
        // and only in the error's last lines, missed it.
        let stale = LineWatch::any_case(&["VERR_already_exists".into(), "is locked".into()]);
        stale.notice("    web: Progress state: VBOX_E_FILE_ERROR");
        assert_eq!(stale.take(), BTreeSet::new());
        stale.notice("VBoxManage: error: Details: code VERR_ALREADY_EXISTS (0x80004005)");
        stale.notice("ok");
        assert_eq!(stale.take().into_iter().collect::<Vec<_>>(), ["verr_already_exists"]);
        assert_eq!(stale.take(), BTreeSet::new(), "taken once");
        stale.notice("the machine is locked");
        assert_eq!(stale.take().len(), 1, "still watched after a take");
        let none = LineWatch::any_case(&[]);
        none.notice("anything");
        assert_eq!(none.take(), BTreeSet::new());
    }

    #[test]
    fn a_watched_text_is_seen_exactly_as_written_until_taken() {
        let watched = LineWatch::default();
        watched.notice("no such IPSet 'iso1n0'");
        assert_eq!(watched.take(), BTreeSet::new(), "nothing watched yet");
        watched.watch(["no such IPSet 'iso1n0'".to_owned(), "no such IPSet 'iso1n1'".to_owned(), String::new()]);
        watched.notice("Error: error getting IPSet content: received an HTTP 500 response - Reason: NO SUCH IPSET 'ISO1N0'");
        watched.notice("Error: error getting IPSet content: received an HTTP 500 response - Reason: no such IPSet 'iso1n00'");
        watched.notice("anything");
        assert_eq!(watched.take(), BTreeSet::new(), "another case, another set: not the text");
        watched.watch(["no such IPSet 'iso1n0'".to_owned()]);
        watched.notice("Error: error getting IPSet content: received an HTTP 500 response - Reason: no such IPSet 'iso1n0'");
        assert_eq!(watched.end().into_iter().collect::<Vec<_>>(), ["no such IPSet 'iso1n0'"]);
        watched.notice("no such IPSet 'iso1n0'");
        assert_eq!(watched.take(), BTreeSet::new(), "ending it ends the watch");
    }
}
