//! The steps an operation goes through, said as `step` events ([`EventKind::Step`]): each format's
//! own phases ([`Phase`]), with what a step is about (a machine, an image) and how far it is
//! ([`StepCount`]). A driver says them from the calls it makes and from what its tool prints.
//!
//! A step is said when it starts, again when its count moves, and once when it ends. What is still
//! open when the operation ends is closed then: done when the operation went through; else the step
//! started last failed (unless a driver already said which one did), and the others were cut short
//! (skipped).

use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::events::{EventKind, StepState};

/// A step of an operation: its id in a `step` event.
///
/// | Format | Steps, in order |
/// |---|---|
/// | Compose | `images` (pulled and built), `create` (networks, volumes, containers), `start`, `health` (waited on until healthy) |
/// | Vagrant | `boxes` (downloaded), then for each machine (the step's `label`): `up`, `provision` |
/// | Terraform | `init`, `apply` (the resources changed so far of those planned), `ready` (the module's provisioners set its machines up) |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Phase {
    /// Compose: the images the services run, pulled and built (counted by image).
    Images,
    /// Compose: the project's networks, volumes and containers made (counted by container).
    Create,
    /// Compose: the containers started (counted by container).
    Start,
    /// Compose: the services waited on until they run healthy, or a one-shot one exited (counted
    /// by container).
    Health,
    /// Vagrant: the boxes the machines boot from, downloaded (counted by box).
    Boxes,
    /// Vagrant: a machine (the label) made and booted.
    Up,
    /// Vagrant: a machine (the label) set up by its provisioners.
    Provision,
    /// Terraform: the module's providers and modules installed, its state's backend readied.
    Init,
    /// Terraform: the module's resources made, changed or removed (counted by resource).
    Apply,
    /// Terraform: the module's provisioners setting its machines up.
    Ready,
}

impl Phase {
    /// Every step, in the order a format goes through its own.
    pub const ALL: [Phase; 10] = [
        Phase::Images,
        Phase::Create,
        Phase::Start,
        Phase::Health,
        Phase::Boxes,
        Phase::Up,
        Phase::Provision,
        Phase::Init,
        Phase::Apply,
        Phase::Ready,
    ];

    /// Its id in a `step` event.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Phase::Images => "images",
            Phase::Create => "create",
            Phase::Start => "start",
            Phase::Health => "health",
            Phase::Boxes => "boxes",
            Phase::Up => "up",
            Phase::Provision => "provision",
            Phase::Init => "init",
            Phase::Apply => "apply",
            Phase::Ready => "ready",
        }
    }

    /// The step whose id is `id`, if it is one.
    #[must_use]
    pub fn of(id: &str) -> Option<Phase> {
        Phase::ALL.into_iter().find(|p| p.id() == id)
    }
}

/// How far a step is: `done` of its `total` items, when the total is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct StepCount {
    /// The items done so far.
    pub done: u32,
    /// How many there are, once the tool says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
}

impl StepCount {
    /// `done` of `total`.
    #[must_use]
    pub fn new(done: u32, total: Option<u32>) -> StepCount {
        StepCount { done, total }
    }
}

/// One step open now.
struct Open {
    phase: Phase,
    label: Option<String>,
    count: Option<StepCount>,
}

/// The steps open now, in the order they started, and whether one was said failed.
#[derive(Default)]
struct Opened {
    steps: Vec<Open>,
    failed: bool,
}

impl Opened {
    fn find(&self, phase: Phase, label: Option<&str>) -> Option<usize> {
        self.steps.iter().position(|s| s.phase == phase && s.label.as_deref() == label)
    }

    fn get_mut(&mut self, phase: Phase, label: Option<&str>) -> Option<&mut Open> {
        self.steps.iter_mut().find(|s| s.phase == phase && s.label.as_deref() == label)
    }
}

/// The steps of one operation, said through `say` as they move.
pub(crate) struct Steps<'a> {
    say: &'a (dyn Fn(EventKind) + Send + Sync),
    opened: Mutex<Opened>,
}

/// Says nothing: the steps of a context that says none (a read, a console, a test's).
fn unsaid(_: EventKind) {}

impl<'a> Steps<'a> {
    /// Steps said through `say`.
    pub(crate) fn new(say: &'a (dyn Fn(EventKind) + Send + Sync)) -> Steps<'a> {
        Steps {
            say,
            opened: Mutex::new(Opened::default()),
        }
    }

    /// Steps said nowhere.
    pub(crate) fn silent() -> Steps<'static> {
        Steps::new(&unsaid)
    }

    fn opened(&self) -> std::sync::MutexGuard<'_, Opened> {
        self.opened.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts step `phase` of `label`: said once, while it isn't open.
    pub(crate) fn start(&self, phase: Phase, label: Option<&str>) {
        let mut opened = self.opened();
        if opened.find(phase, label).is_none() {
            opened.steps.push(Open {
                phase,
                label: label.map(str::to_owned),
                count: None,
            });
            self.said(phase, label, StepState::Started, None);
        }
    }

    /// Step `phase` of `label` is `count` along: said when the count moved (the step started
    /// first when it isn't open).
    pub(crate) fn count(&self, phase: Phase, label: Option<&str>, count: StepCount) {
        let mut opened = self.opened();
        match opened.get_mut(phase, label) {
            Some(open) if open.count == Some(count) => return,
            Some(open) => open.count = Some(count),
            None => opened.steps.push(Open {
                phase,
                label: label.map(str::to_owned),
                count: Some(count),
            }),
        }
        self.said(phase, label, StepState::Started, Some(count));
    }

    /// How far open step `phase` of `label` is, when it said a count.
    pub(crate) fn count_of(&self, phase: Phase, label: Option<&str>) -> Option<StepCount> {
        self.opened().get_mut(phase, label).and_then(|open| open.count)
    }

    /// Whether step `phase` of `label` is open.
    pub(crate) fn is_open(&self, phase: Phase, label: Option<&str>) -> bool {
        self.opened().find(phase, label).is_some()
    }

    /// Ends step `phase` of `label` as done, when it is open.
    pub(crate) fn done(&self, phase: Phase, label: Option<&str>) {
        self.close(phase, label, StepState::Done);
    }

    /// Ends step `phase` of `label` as failed, when it is open: the operation's failure is its.
    pub(crate) fn failed(&self, phase: Phase, label: Option<&str>) {
        self.close(phase, label, StepState::Failed);
    }

    fn close(&self, phase: Phase, label: Option<&str>, state: StepState) {
        let mut opened = self.opened();
        if let Some(at) = opened.find(phase, label) {
            let step = opened.steps.remove(at);
            opened.failed |= state == StepState::Failed;
            self.said(step.phase, label, state, step.count);
        }
    }

    /// Ends every open step: done when `ok`; else the one started last failed (unless one was
    /// already said failed), the others cut short.
    pub(crate) fn end(&self, ok: bool) {
        let mut opened = self.opened();
        let steps = std::mem::take(&mut opened.steps);
        let blamed = if ok || opened.failed { None } else { steps.len().checked_sub(1) };
        opened.failed |= blamed.is_some();
        for (at, step) in steps.into_iter().enumerate() {
            let state = if ok {
                StepState::Done
            } else if Some(at) == blamed {
                StepState::Failed
            } else {
                StepState::Skipped
            };
            self.said(step.phase, step.label.as_deref(), state, step.count);
        }
    }

    fn said(&self, phase: Phase, label: Option<&str>, state: StepState, count: Option<StepCount>) {
        (self.say)(EventKind::Step {
            step: phase.id().to_owned(),
            state,
            label: label.map(str::to_owned),
            count,
        });
    }
}

/// The steps said through a [`Steps`], kept, for tests.
#[cfg(test)]
pub(crate) mod said {
    use std::sync::{Mutex, PoisonError};

    use crate::events::{EventKind, StepState};

    /// The step events said, kept in order.
    #[derive(Default)]
    pub(crate) struct Said(Mutex<Vec<EventKind>>);

    impl Said {
        /// Keeps `kind`.
        pub(crate) fn keep(&self, kind: EventKind) {
            self.0.lock().unwrap_or_else(PoisonError::into_inner).push(kind);
        }

        /// Each step said, as `step[:label] state[ done/total]`.
        pub(crate) fn lines(&self) -> Vec<String> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter_map(|kind| match kind {
                    EventKind::Step { step, state, label, count } => {
                        let state = match state {
                            StepState::Started => "started",
                            StepState::Done => "done",
                            StepState::Failed => "failed",
                            StepState::Skipped => "skipped",
                        };
                        let label = label.as_deref().map(|l| format!(":{l}")).unwrap_or_default();
                        let count = count
                            .map(|c| format!(" {}/{}", c.done, c.total.map_or_else(|| "?".to_owned(), |t| t.to_string())))
                            .unwrap_or_default();
                        Some(format!("{step}{label} {state}{count}"))
                    }
                    _ => None,
                })
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::said::Said;
    use super::*;

    #[test]
    fn each_id_reads_back_as_its_step() {
        for phase in Phase::ALL {
            assert_eq!(Phase::of(phase.id()), Some(phase));
        }
        assert_eq!(Phase::of("pull-up"), None);
    }

    #[test]
    fn a_step_is_said_when_it_starts_moves_and_ends_once() {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let s = Steps::new(&say);
        s.start(Phase::Images, None);
        s.start(Phase::Images, None);
        s.count(Phase::Images, None, StepCount::new(1, Some(2)));
        s.count(Phase::Images, None, StepCount::new(1, Some(2)));
        s.count(Phase::Images, None, StepCount::new(2, Some(2)));
        s.done(Phase::Images, None);
        s.done(Phase::Images, None);
        s.count(Phase::Create, None, StepCount::new(0, None));
        s.end(true);
        assert_eq!(
            said.lines(),
            [
                "images started",
                "images started 1/2",
                "images started 2/2",
                "images done 2/2",
                "create started 0/?",
                "create done 0/?",
            ]
        );
    }

    #[test]
    fn a_failed_operation_fails_the_step_started_last_and_cuts_the_others_short() {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let s = Steps::new(&say);
        s.start(Phase::Up, Some("dc01"));
        s.start(Phase::Up, Some("ws01"));
        s.end(false);
        assert_eq!(said.lines(), ["up:dc01 started", "up:ws01 started", "up:dc01 skipped", "up:ws01 failed"]);
    }

    #[test]
    fn a_step_said_failed_is_the_failure_and_the_others_are_cut_short() {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let s = Steps::new(&say);
        s.start(Phase::Up, Some("dc01"));
        s.start(Phase::Up, Some("ws01"));
        s.failed(Phase::Up, Some("dc01"));
        s.end(false);
        s.end(false);
        assert_eq!(said.lines(), ["up:dc01 started", "up:ws01 started", "up:dc01 failed", "up:ws01 skipped"]);
    }
}
