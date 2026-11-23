//! What an `up`'s lines say of its steps ([`Phase`]): the boxes it downloads (`boxes`, counted by
//! box), then each machine's `up` and `provision`, the machine as the step's label. Vagrant says
//! which machine a line is of (`==> dc01: Booting VM...`, `    dc01: Progress: 42%`): a machine's
//! `up` begins at its first line that isn't of a box's download, its `provision` at its first
//! provisioner (its `up` then done). A failure that names its machine (`…while executing the
//! action on the 'dc01' machine`) fails that machine's step.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use crate::steps::{Phase, StepCount, Steps};

/// The machine `line` is of, and what it says of it: `==> name: …` or `    name: …`.
fn of_machine(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("==>").map_or_else(|| line.strip_prefix("    "), Some)?;
    let (name, said) = rest.trim_start().split_once(':')?;
    let named = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    named.then(|| (name, said.trim()))
}

/// The first name `text` quotes (`Adding box 'bento/debian-12' (v1)` → `bento/debian-12`).
fn quoted(text: &str) -> Option<&str> {
    let (_, rest) = text.split_once('\'')?;
    rest.split_once('\'').map(|(name, _)| name)
}

/// What a machine's line says of a box being downloaded for it.
enum OfBox<'a> {
    /// A box it needs is fetched.
    Fetching(&'a str),
    /// The box was added.
    Added(&'a str),
    /// More of the download (its URL, its progress).
    Downloading,
}

fn of_box(said: &str) -> Option<OfBox<'_>> {
    if (said.starts_with("Box '") && said.contains("could not be found")) || said.starts_with("Adding box '") {
        return quoted(said).map(OfBox::Fetching);
    }
    if said.starts_with("Successfully added box '") {
        return quoted(said).map(OfBox::Added);
    }
    let downloading = ["Loading metadata for box", "Downloading:", "Progress:", "Box Provider:", "Box Version:", "URL:"];
    downloading.iter().any(|d| said.starts_with(d)).then_some(OfBox::Downloading)
}

/// The boxes fetched so far, and those added.
#[derive(Default)]
struct Boxes {
    fetching: BTreeSet<String>,
    added: BTreeSet<String>,
}

impl Boxes {
    fn count(&self) -> StepCount {
        let n = |set: &BTreeSet<String>| u32::try_from(set.len()).unwrap_or(u32::MAX);
        StepCount::new(n(&self.added), Some(n(&self.fetching)))
    }
}

/// The steps a Vagrant call's lines say, said to `steps`.
pub(super) struct Watch<'s, 'a> {
    steps: &'s Steps<'a>,
    boxes: Mutex<Boxes>,
}

impl<'s, 'a> Watch<'s, 'a> {
    /// A watch of an `up`, its steps said to `steps`.
    pub(super) fn new(steps: &'s Steps<'a>) -> Watch<'s, 'a> {
        Watch {
            steps,
            boxes: Mutex::new(Boxes::default()),
        }
    }

    /// Reads one line the call printed.
    pub(super) fn see(&self, line: &str) {
        if let Some((machine, _)) = line.split_once("executing the action on the '").and_then(|(_, rest)| rest.split_once('\'')) {
            let failed = if self.steps.is_open(Phase::Provision, Some(machine)) {
                Phase::Provision
            } else {
                Phase::Up
            };
            self.steps.failed(failed, Some(machine));
            return;
        }
        let Some((machine, said)) = of_machine(line) else { return };
        if let Some(of) = of_box(said) {
            self.box_moved(&of);
            return;
        }
        if said.starts_with("Running provisioner:") {
            self.steps.done(Phase::Up, Some(machine));
            self.steps.start(Phase::Provision, Some(machine));
        } else if !self.steps.is_open(Phase::Provision, Some(machine)) {
            self.steps.start(Phase::Up, Some(machine));
        }
    }

    fn box_moved(&self, of: &OfBox<'_>) {
        let mut boxes = self.boxes.lock().unwrap_or_else(PoisonError::into_inner);
        match of {
            OfBox::Fetching(name) => {
                boxes.fetching.insert((*name).to_owned());
            }
            OfBox::Added(name) => {
                boxes.fetching.insert((*name).to_owned());
                boxes.added.insert((*name).to_owned());
            }
            OfBox::Downloading => return,
        }
        let count = boxes.count();
        self.steps.count(Phase::Boxes, None, count);
        if count.total == Some(count.done) {
            self.steps.done(Phase::Boxes, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;
    use crate::steps::said::Said;

    fn watched(lines: &str, ok: bool) -> Vec<String> {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let steps = Steps::new(&say);
        let watch = Watch::new(&steps);
        lines.lines().for_each(|line| watch.see(line));
        steps.end(ok);
        said.lines()
    }

    #[test]
    fn a_line_names_its_machine() {
        assert_eq!(of_machine("==> dc01: Booting VM..."), Some(("dc01", "Booting VM...")));
        assert_eq!(of_machine("    web-01.example: Progress: 42%"), Some(("web-01.example", "Progress: 42%")));
        assert_eq!(of_machine("==>dc01: odd"), Some(("dc01", "odd")));
        assert_eq!(of_machine("Bringing machine 'dc01' up with 'virtualbox' provider..."), None);
        assert_eq!(of_machine("    at least: two words"), None);
    }

    #[test]
    fn the_boxes_come_first_then_each_machine_boots_and_is_provisioned() {
        let up = "Bringing machine 'dc01' up with 'virtualbox' provider...
Bringing machine 'ws01' up with 'virtualbox' provider...
==> dc01: Box 'bento/debian-12' could not be found. Attempting to find and install...
    dc01: Box Provider: virtualbox
==> dc01: Loading metadata for box 'bento/debian-12'
==> dc01: Adding box 'bento/debian-12' (v202407.22.0) for provider: virtualbox (amd64)
    dc01: Downloading: https://example.test/debian.box
    dc01: Progress: 42% (Rate: 1201k/s, Estimated time remaining: 0:04:10)
==> dc01: Successfully added box 'bento/debian-12' (v202407.22.0) for 'virtualbox (amd64)'!
==> dc01: Importing base box 'bento/debian-12'...
==> ws01: Importing base box 'bento/debian-12'...
==> dc01: Booting VM...
==> dc01: Running provisioner: shell...
    dc01: Running: inline script
==> ws01: Booting VM...
==> ws01: Machine booted and ready!";
        assert_eq!(
            watched(up, true),
            [
                "boxes started 0/1",
                "boxes started 1/1",
                "boxes done 1/1",
                "up:dc01 started",
                "up:ws01 started",
                "up:dc01 done",
                "provision:dc01 started",
                // What is still open ends with the call, in the order it started.
                "up:ws01 done",
                "provision:dc01 done",
            ]
        );
    }

    #[test]
    fn a_failure_that_names_its_machine_fails_its_step_and_cuts_the_others_short() {
        let up = "==> dc01: Booting VM...
==> ws01: Booting VM...
==> dc01: Running provisioner: shell...
==> ws01: Machine booted and ready!
An error occurred while executing the action on the 'dc01' machine. Please handle this error then try again:";
        assert_eq!(
            watched(up, false),
            [
                "up:dc01 started",
                "up:ws01 started",
                "up:dc01 done",
                "provision:dc01 started",
                "provision:dc01 failed",
                "up:ws01 skipped",
            ]
        );
    }
}
