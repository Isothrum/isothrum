//! A whole setup run on this computer, step by step ([`walk`]): each step of the plan run by the
//! caller's own runner of it, the machine checked again after each (an install changes what comes
//! next: Docker installed asks to start it, or for a new login). What a UI does one click at a
//! time, for a caller without one: a command line, a headless setup.

use serde::Serialize;

use super::{Action, Machine, Options, Step, Wanted, plan};

/// What a walk tells as it goes; `O`: how the caller's runner says a step ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum Event<O> {
    /// A step begins: its item, its kind ([`Action::kind`]) and what it does.
    Started {
        /// The item it is about (`docker`, `engine:docker-engine`, `virtualbox`, …).
        item: String,
        /// Its kind.
        kind: &'static str,
        /// The action.
        action: Action,
    },
    /// A line of the step's output.
    Line {
        /// The step's item.
        item: String,
        /// The line.
        line: String,
    },
    /// The step ended.
    Ended {
        /// The step's item.
        item: String,
        /// How.
        #[serde(flatten)]
        outcome: O,
    },
}

/// What a walk does once a step failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OnFailure {
    /// Stop there: a caller who fixes it and runs it again.
    Stop,
    /// Go on with the other steps: a pass that reports every item.
    Continue,
}

/// Where a walk ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Walked {
    /// The restart or the new login that ends the pass, when one does: the next pass picks up
    /// after it.
    pub ends: Option<Step>,
    /// The items of the steps run, in order.
    pub acted: Vec<String>,
    /// The first step that failed, when one did.
    pub failed: Option<Step>,
}

/// How a walk runs its steps: the caller's own runner of each, and its machine check.
pub struct Steps<R, F, C> {
    /// Runs a step, its output lines into the sink it is given: how it ended.
    pub run: R,
    /// Whether how a step ended is a failure.
    pub failed: F,
    /// Checks the machine again: what it found, with its options.
    pub check: C,
}

/// Walks the setup `wanted` from `machine` and its options: the next step of the plan not done
/// yet (a restart or a new login last, never run) run by `steps`, told to `on`, the machine
/// checked again after it, until none is left, or (`on_failure`) one fails.
pub async fn walk<O, R, F, C>(
    wanted: &Wanted,
    mut machine: (Machine, Options),
    on_failure: OnFailure,
    mut steps: Steps<R, F, C>,
    mut on: impl FnMut(Event<O>),
) -> Walked
where
    R: AsyncFnMut(&Step, &mut dyn FnMut(String)) -> O,
    F: Fn(&O) -> bool,
    C: AsyncFnMut() -> (Machine, Options),
{
    let mut done: Vec<Step> = Vec::new();
    let mut first_failed = None;
    loop {
        let p = plan(&machine.0, &machine.1, wanted);
        let next = p.steps.iter().find(|s| !s.action.ends_pass() && !done.contains(s)).cloned();
        let Some(step) = next.filter(|_| first_failed.is_none() || on_failure == OnFailure::Continue) else {
            let acted = done.into_iter().map(|s| s.item).collect();
            return Walked {
                ends: p.steps.into_iter().find(|s| s.action.ends_pass()),
                acted,
                failed: first_failed,
            };
        };
        on(Event::Started {
            item: step.item.clone(),
            kind: step.action.kind(),
            action: step.action.clone(),
        });
        let outcome = {
            let mut line = |line| {
                on(Event::Line { item: step.item.clone(), line });
            };
            (steps.run)(&step, &mut line).await
        };
        if (steps.failed)(&outcome) && first_failed.is_none() {
            first_failed = Some(step.clone());
        }
        on(Event::Ended {
            item: step.item.clone(),
            outcome,
        });
        done.push(step);
        machine = (steps.check)().await;
    }
}
