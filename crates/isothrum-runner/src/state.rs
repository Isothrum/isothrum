//! The words for what a run is doing and where it stands, shared by the API, the record and the
//! event stream.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An operation on a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Op {
    /// Start the run.
    Up,
    /// Remove everything the run created.
    Down,
    /// Stop the run, keeping what it created.
    Park,
    /// Start a parked run again.
    Resume,
    /// Run the run's checks.
    Check,
    /// Run the set-up again on running machines.
    Provision,
    /// Hot-add the on-demand access machine into the running run.
    Attach,
    /// Hot-remove the on-demand access machine from the running run.
    Detach,
    /// Bring the running run up to date with its manifest.
    Update,
    /// Take a machine out of the running run, ahead of its removal from the manifest.
    Retire,
}

impl Op {
    /// The operation as written in events (`up`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Up => "up",
            Op::Down => "down",
            Op::Park => "park",
            Op::Resume => "resume",
            Op::Check => "check",
            Op::Provision => "provision",
            Op::Attach => "attach",
            Op::Detach => "detach",
            Op::Update => "update",
            Op::Retire => "retire",
        }
    }

    /// The state a run is in while this operation runs.
    pub(crate) fn during(self) -> Option<RunState> {
        match self {
            Op::Up => Some(RunState::Starting),
            Op::Down => Some(RunState::Stopping),
            Op::Park => Some(RunState::Parking),
            Op::Resume => Some(RunState::Resuming),
            // An update changes a run that keeps running: it stays ready meanwhile.
            Op::Check | Op::Provision | Op::Attach | Op::Detach | Op::Update | Op::Retire => None,
        }
    }
}

impl fmt::Display for Op {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a run stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunState {
    /// Being started.
    Starting,
    /// Up, every member machine running.
    Ready,
    /// Something of it exists, but not every member machine runs (a failed start, a machine down).
    Failed,
    /// Being parked.
    Parking,
    /// Parked: stopped, what it created kept.
    Parked,
    /// Being resumed.
    Resuming,
    /// Being removed.
    Stopping,
    /// Nothing of it exists.
    Absent,
}

impl RunState {
    /// The state as written in events (`ready`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Starting => "starting",
            RunState::Ready => "ready",
            RunState::Failed => "failed",
            RunState::Parking => "parking",
            RunState::Parked => "parked",
            RunState::Resuming => "resuming",
            RunState::Stopping => "stopping",
            RunState::Absent => "absent",
        }
    }
}

impl fmt::Display for RunState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a run is parked: the manifest's `lifecycle.park.pause` or `.shutdown` steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Park {
    /// Suspended, memory kept where the target can (Docker: stopped).
    Pause,
    /// Shut down.
    Shutdown,
}

impl Park {
    /// The mode as the manifest names it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Park::Pause => "pause",
            Park::Shutdown => "shutdown",
        }
    }
}

impl fmt::Display for Park {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_state_and_park_mode_is_written_as_serde_writes_it() {
        crate::testing::assert_named_as_serde(Op::as_str);
        crate::testing::assert_named_as_serde(RunState::as_str);
        crate::testing::assert_named_as_serde(Park::as_str);
    }
}
