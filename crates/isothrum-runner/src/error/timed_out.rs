//! Which limit a step that ran out of time ran into.

use serde::{Deserialize, Serialize};

/// Which limit a step ran into: a timeout's, also the `error` event's `timeout`.
///
/// A step's limit (its `timeout_s`) counts the time it shows no progress: progress is an output
/// line, on either stream, that isn't blank and isn't one of the last distinct lines it printed
/// (a stuck download repeating `Downloading 1.121GB` makes none). A step that prints nothing has
/// its limit from its start. A step that keeps making progress still stops at a ceiling: six
/// hours from its start, or its limit when that is longer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TimedOut {
    /// It showed no progress for its limit.
    NoProgress {
        /// The limit, in seconds.
        after_s: u64,
        /// The last line it printed (secrets masked, cut to 300 bytes); none when it printed
        /// nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_line: Option<String>,
    },
    /// It was still making progress at the ceiling.
    Ceiling {
        /// The ceiling, in seconds from the step's start.
        after_s: u64,
    },
}

impl TimedOut {
    /// The limit it ran into, in seconds.
    #[must_use]
    pub fn after_s(&self) -> u64 {
        match self {
            TimedOut::NoProgress { after_s, .. } | TimedOut::Ceiling { after_s } => *after_s,
        }
    }

    /// The same, its last line passed through `mask`.
    pub(crate) fn masked(self, mask: impl FnOnce(&str) -> String) -> TimedOut {
        match self {
            TimedOut::NoProgress { after_s, last_line } => TimedOut::NoProgress {
                after_s,
                last_line: last_line.map(|l| mask(&l)),
            },
            ceiling @ TimedOut::Ceiling { .. } => ceiling,
        }
    }
}

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TimedOut::NoProgress {
                after_s,
                last_line: Some(line),
            } => {
                write!(f, "showed no progress for {after_s}s and was stopped; its last line: `{line}`")
            }
            TimedOut::NoProgress { after_s, last_line: None } => write!(f, "printed nothing for {after_s}s and was stopped"),
            TimedOut::Ceiling { after_s } => {
                write!(f, "was stopped after {after_s}s, the most a step may run, though it kept making progress")
            }
        }
    }
}
