//! What a start's Compose calls say of its steps ([`Phase`]): `images` from its pull on (the
//! builds its `up` makes counted in), then `create`, `start` and `health` from the lines its `up`
//! prints, each object's name and what happens to it (`Container web-1  Created`). Images are
//! counted by image, the rest by container.
//!
//! A step ends as the next one begins. A container waited on while the others still start (a
//! service that waits for another's health) is part of `start`: `health` begins once every
//! container started.

use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};

use crate::steps::{Phase, StepCount, Steps};

/// The kinds of object Compose names before one's name.
const KINDS: [&str; 5] = ["Container", "Network", "Volume", "Image", "Service"];

/// A line Compose prints of one object: its kind (when it names one), its name and what happens
/// to it (`Container web-1  Started`, `web Pulling`).
fn said(line: &str) -> Option<(Option<&str>, &str, &str)> {
    // A terminal's marks (`✔`, `✘`) are no word of the line.
    let mut words = line.split_whitespace().filter(|w| w.chars().any(char::is_alphanumeric));
    let first = words.next()?;
    let (kind, name) = if KINDS.contains(&first) {
        (Some(first), words.next()?)
    } else {
        (None, first)
    };
    Some((kind, name, words.next()?))
}

/// What a line says moved, read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moved {
    /// An image's pull or build began.
    Fetching,
    /// An image was pulled or built (or another service's pull fetches it).
    Fetched,
    /// An object is being made.
    Creating,
    /// It was made.
    Created,
    /// A container is starting.
    Starting,
    /// It started (or ran already, made as asked).
    Started,
    /// A container is waited on.
    Waiting,
    /// It runs healthy, or ended (a one-shot service).
    Settled,
}

/// What `status` says moved, if it says anything of the steps.
fn moved(status: &str) -> Option<Moved> {
    Some(match status {
        "Pulling" | "Building" => Moved::Fetching,
        "Pulled" | "Built" | "Skipped" => Moved::Fetched,
        "Creating" | "Recreate" => Moved::Creating,
        "Created" | "Recreated" => Moved::Created,
        "Starting" => Moved::Starting,
        "Started" | "Running" => Moved::Started,
        "Waiting" => Moved::Waiting,
        "Healthy" | "Exited" => Moved::Settled,
        _ => return None,
    })
}

/// The names that began and ended something, counted as one step's.
#[derive(Default)]
struct Tally {
    began: BTreeSet<String>,
    ended: BTreeSet<String>,
}

impl Tally {
    fn began(&mut self, name: &str) {
        self.began.insert(name.to_owned());
    }

    fn ended(&mut self, name: &str) {
        self.began(name);
        self.ended.insert(name.to_owned());
    }

    /// Done of total, counted from `base`.
    fn count(&self, base: StepCount) -> StepCount {
        let n = |set: &BTreeSet<String>| u32::try_from(set.len()).unwrap_or(u32::MAX);
        StepCount::new(base.done + n(&self.ended), Some(base.total.unwrap_or(base.done) + n(&self.began)))
    }
}

/// What the lines read so far said.
#[derive(Default)]
struct Read {
    /// The step the start is at, once one began.
    at: Option<Phase>,
    /// The images count the images step had when this call began (its pull's).
    base: Option<StepCount>,
    images: Tally,
    create: Tally,
    start: Tally,
    health: Tally,
}

/// The steps a Compose call's lines say, said to `steps`.
pub(super) struct Watch<'s, 'a> {
    steps: &'s Steps<'a>,
    read: Mutex<Read>,
}

impl<'s, 'a> Watch<'s, 'a> {
    /// A watch of a pull or an `up`, its steps said to `steps`.
    pub(super) fn new(steps: &'s Steps<'a>) -> Watch<'s, 'a> {
        Watch {
            steps,
            read: Mutex::new(Read::default()),
        }
    }

    /// The images step begins: before a pull.
    pub(super) fn pulling(&self) {
        self.steps.start(Phase::Images, None);
        self.read.lock().unwrap_or_else(PoisonError::into_inner).at = Some(Phase::Images);
    }

    /// Reads one line the call printed.
    pub(super) fn see(&self, line: &str) {
        let Some((kind, name, status)) = said(line) else { return };
        let Some(moved) = moved(status) else { return };
        let mut read = self.read.lock().unwrap_or_else(PoisonError::into_inner);
        let container = kind.is_none_or(|k| k == "Container");
        let phase = match moved {
            Moved::Fetching | Moved::Fetched => Phase::Images,
            Moved::Creating | Moved::Created => Phase::Create,
            Moved::Waiting | Moved::Settled if read.at == Some(Phase::Health) || every_one_started(&read) => Phase::Health,
            // Starting, and a container waited on before every one started: a dependency's,
            // waited on by the start.
            _ => Phase::Start,
        };
        let tally = match phase {
            Phase::Images => &mut read.images,
            Phase::Create if container => &mut read.create,
            Phase::Start => &mut read.start,
            Phase::Health => &mut read.health,
            // A network or volume made: part of `create`, not counted.
            _ => {
                self.reach(&mut read, phase);
                return;
            }
        };
        match moved {
            Moved::Fetching | Moved::Creating | Moved::Starting | Moved::Waiting => tally.began(name),
            Moved::Fetched | Moved::Created | Moved::Started | Moved::Settled => tally.ended(name),
        }
        if phase == Phase::Start && moved == Moved::Started {
            // A container that runs was made, by this call or before it.
            read.create.ended(name);
        }
        self.reach(&mut read, phase);
        if read.at == Some(phase) {
            let count = self.count_of(&mut read, phase);
            self.steps.count(phase, None, count);
        }
    }

    /// The count of step `phase` as read.
    fn count_of(&self, read: &mut Read, phase: Phase) -> StepCount {
        match phase {
            Phase::Images => {
                // The builds of an `up` add to the pull's count.
                let base = *read
                    .base
                    .get_or_insert_with(|| self.steps.count_of(Phase::Images, None).unwrap_or(StepCount::new(0, Some(0))));
                read.images.count(base)
            }
            Phase::Create => read.create.count(StepCount::new(0, Some(0))),
            Phase::Start => {
                let mut count = read.start.count(StepCount::new(0, Some(0)));
                count.total = Some(count.total.unwrap_or(0).max(u32::try_from(read.create.began.len()).unwrap_or(u32::MAX)));
                count
            }
            _ => read.health.count(StepCount::new(0, Some(0))),
        }
    }

    /// The start is at `phase` from now on, when it is past the step it was at: the steps before
    /// it are done.
    fn reach(&self, read: &mut Read, phase: Phase) {
        let order = |p: Phase| [Phase::Images, Phase::Create, Phase::Start, Phase::Health].iter().position(|q| *q == p);
        if read.at.is_some_and(|at| order(at) >= order(phase)) {
            return;
        }
        for before in [Phase::Images, Phase::Create, Phase::Start, Phase::Health] {
            if order(before) >= order(phase) {
                break;
            }
            self.steps.done(before, None);
        }
        read.at = Some(phase);
        self.steps.start(phase, None);
    }
}

/// Whether every container made so far started.
fn every_one_started(read: &Read) -> bool {
    !read.start.ended.is_empty() && read.create.began.iter().all(|c| read.start.ended.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventKind;
    use crate::steps::said::Said;

    fn watched(lines: &[&str]) -> Vec<String> {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let steps = Steps::new(&say);
        let watch = Watch::new(&steps);
        watch.pulling();
        for line in lines {
            watch.see(line);
        }
        steps.end(true);
        said.lines()
    }

    #[test]
    fn a_line_names_an_object_and_what_happens_to_it() {
        assert_eq!(said(" Container web-1  Started"), Some((Some("Container"), "web-1", "Started")));
        assert_eq!(said(" ✔ Network stack_default  Created"), Some((Some("Network"), "stack_default", "Created")));
        assert_eq!(said(" web Pulling "), Some((None, "web", "Pulling")));
        assert_eq!(said("36b0bafd7f97 Downloading [==>   ] 1.2MB/3MB").map(|s| s.2), Some("Downloading"));
        assert_eq!(said(""), None);
    }

    #[test]
    fn a_start_goes_through_its_images_then_makes_starts_and_waits_on_its_containers() {
        assert_eq!(
            watched(&[
                " web Pulling ",
                " db Pulling ",
                "36b0bafd7f97 Downloading [==>   ] 1.2MB/3MB",
                " db Pulled ",
                " web Pulled ",
                " Network stack_default  Creating",
                " Network stack_default  Created",
                " Container stack-db-1  Creating",
                " Container stack-web-1  Creating",
                " Container stack-db-1  Created",
                " Container stack-web-1  Created",
                " Container stack-db-1  Starting",
                " Container stack-db-1  Started",
                // web waits for db's health before it starts: still the start.
                " Container stack-db-1  Waiting",
                " Container stack-db-1  Healthy",
                " Container stack-web-1  Starting",
                " Container stack-web-1  Started",
                " Container stack-web-1  Waiting",
                " Container stack-db-1  Waiting",
                " Container stack-db-1  Healthy",
                " Container stack-web-1  Healthy",
            ]),
            [
                "images started",
                "images started 0/1",
                "images started 0/2",
                "images started 1/2",
                "images started 2/2",
                "images done 2/2",
                "create started",
                "create started 0/1",
                "create started 0/2",
                "create started 1/2",
                "create started 2/2",
                "create done 2/2",
                "start started",
                "start started 0/2",
                "start started 1/2",
                "start started 2/2",
                "start done 2/2",
                "health started",
                "health started 0/1",
                "health started 0/2",
                "health started 1/2",
                "health started 2/2",
                "health done 2/2",
            ]
        );
    }

    #[test]
    fn the_builds_of_an_up_add_to_the_images_its_pull_fetched() {
        let said = Said::default();
        let say = |kind: EventKind| said.keep(kind);
        let steps = Steps::new(&say);
        let pull = Watch::new(&steps);
        pull.pulling();
        pull.see(" db Pulling ");
        pull.see(" db Pulled ");
        let up = Watch::new(&steps);
        up.see(" Service web  Building");
        up.see(" Service web  Built");
        up.see(" Container stack-web-1  Running");
        steps.end(true);
        assert_eq!(
            said.lines(),
            [
                "images started",
                "images started 0/1",
                "images started 1/1",
                "images started 1/2",
                "images started 2/2",
                "images done 2/2",
                // A container made before, as it is asked to be.
                "start started",
                "start started 1/1",
                "start done 1/1",
            ]
        );
    }

    #[test]
    fn an_up_with_nothing_to_pull_still_says_its_images_step() {
        assert_eq!(watched(&[]), ["images started", "images done"]);
    }
}
