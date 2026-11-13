//! The run's state machine: every move a run can make, as a [`Transition`] whose only
//! constructors check it against the table below, so the store can't be handed an illegal one.
//!
//! | Op | From | During | Success | Failure |
//! |---|---|---|---|---|
//! | `up` | absent, failed (ready, parked: replaced) | creating | ready | failed |
//! | `resume` | parked | resuming | ready | failed |
//! | `park` | ready | parking | parked | failed |
//! | `update` | ready | updating | ready | failed |
//! | `down` | ready, parked, failed, absent | destroying | absent | failed |
//!
//! A recovery moves a run caught in a moving state (its operation's process died) to `failed`,
//! `cancelled`. An observation moves a settled run to what its machines show (never a moving
//! one: an operation owns it). `check`, `provision`, `attach`, `detach` and `retire` don't move
//! the run.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{Op, RunState};
use crate::error::ErrorCode;
use crate::store::OperationId;

/// What a user wants of a run: set by the commands, never by an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Desired {
    /// Up and ready.
    Running,
    /// Parked: stopped, what it created kept.
    Parked,
    /// Nothing of it left.
    Gone,
}

impl Desired {
    /// What a successful `op` leaves the user wanting; None for an op that doesn't change it.
    #[must_use]
    pub fn after(op: Op) -> Option<Desired> {
        match op {
            Op::Up | Op::Resume => Some(Desired::Running),
            Op::Park => Some(Desired::Parked),
            Op::Down => Some(Desired::Gone),
            Op::Check | Op::Provision | Op::Attach | Op::Detach | Op::Update | Op::Retire => None,
        }
    }

    /// The settled state that meets this wish.
    #[must_use]
    pub fn met_by(self) -> RunState {
        match self {
            Desired::Running => RunState::Ready,
            Desired::Parked => RunState::Parked,
            Desired::Gone => RunState::Absent,
        }
    }

    /// The word as written in the store and the events (`running`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Desired::Running => "running",
            Desired::Parked => "parked",
            Desired::Gone => "gone",
        }
    }
}

impl fmt::Display for Desired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What moved a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "by", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Cause {
    /// An operation, by its journal id.
    Operation {
        /// The operation's id.
        id: OperationId,
        /// What it is.
        op: Op,
    },
    /// A status read: what the engine showed.
    Observation,
    /// The operation `id`, finding the run left moving by an operation whose process died.
    Recovery {
        /// The recovering operation's id.
        id: OperationId,
    },
}

impl Cause {
    /// The operation behind the move, if one is.
    #[must_use]
    pub fn operation(self) -> Option<OperationId> {
        match self {
            Cause::Operation { id, .. } | Cause::Recovery { id } => Some(id),
            Cause::Observation => None,
        }
    }
}

/// A move the table doesn't allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a run {from} can't {what}")]
pub struct IllegalTransition {
    /// Where the run stood.
    pub from: RunState,
    /// What was asked (`be parked`, `become ready by an observation`).
    pub what: &'static str,
}

/// A legal move of a run, from one state to another, with what moved it. Its fields are
/// private: the constructors are the only way to one, and each checks the move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transition {
    from: RunState,
    to: RunState,
    cause: Option<ErrorCode>,
    by: Cause,
}

/// The states `op` may begin from, and what asking it is called in an [`IllegalTransition`].
fn begins_from(op: Op) -> (&'static [RunState], &'static str) {
    use RunState::{Absent, Failed, Parked, Ready};
    match op {
        // A replacing `up` (or one over a failed start) removes what is there first: the op's own
        // checks refuse a running run (`already_running`), not the table.
        Op::Up => (&[Absent, Failed, Ready, Parked], "be brought up"),
        Op::Resume => (&[Parked], "be resumed"),
        Op::Park => (&[Ready], "be parked"),
        Op::Update => (&[Ready], "be updated"),
        Op::Down => (&[Ready, Parked, Failed, Absent], "be brought down"),
        Op::Check | Op::Provision | Op::Attach | Op::Detach | Op::Retire => (&[], "be moved by this operation"),
    }
}

/// Whether an observation may move a settled run from `from` to `to`.
fn observable(from: RunState, to: RunState) -> bool {
    use RunState::{Absent, Failed, Parked, Ready};
    matches!(
        (from, to),
        (Ready, Failed | Absent | Parked) | (Failed, Ready | Absent) | (Parked, Ready | Absent | Failed)
    )
}

impl Transition {
    /// Operation `op` (journal id `id`) begins acting on a run in `from`: the run moves to the
    /// state the op holds it in.
    ///
    /// # Errors
    ///
    /// The table has no such move: the op doesn't move a run, or not from `from`.
    pub fn begin(from: RunState, op: Op, id: OperationId) -> Result<Transition, IllegalTransition> {
        let (allowed, what) = begins_from(op);
        match op.during() {
            Some(to) if allowed.contains(&from) => Ok(Transition {
                from,
                to,
                cause: None,
                by: Cause::Operation { id, op },
            }),
            _ => Err(IllegalTransition { from, what }),
        }
    }

    /// The operation that made `begun` reached its goal: the run settles where the op leaves it.
    #[must_use]
    pub fn succeed(begun: Transition) -> Transition {
        let to = match begun.to {
            RunState::Parking => RunState::Parked,
            RunState::Destroying => RunState::Absent,
            // Creating, Resuming, Updating: and nothing else is a `begin`'s target.
            _ => RunState::Ready,
        };
        Transition {
            from: begun.to,
            to,
            cause: None,
            by: begun.by,
        }
    }

    /// The operation that made `begun` failed with `cause` (`cancelled` when cut short).
    #[must_use]
    pub fn fail(begun: Transition, cause: ErrorCode) -> Transition {
        Transition {
            from: begun.to,
            to: RunState::Failed,
            cause: Some(cause),
            by: begun.by,
        }
    }

    /// Operation `id` found the run in `from`, a moving state whose operation's process died: it
    /// failed, `cancelled`.
    ///
    /// # Errors
    ///
    /// `from` isn't a moving state.
    pub fn cut_off(from: RunState, id: OperationId) -> Result<Transition, IllegalTransition> {
        if !from.is_moving() {
            return Err(IllegalTransition {
                from,
                what: "be recovered (it isn't moving)",
            });
        }
        Ok(Transition {
            from,
            to: RunState::Failed,
            cause: Some(ErrorCode::Cancelled),
            by: Cause::Recovery { id },
        })
    }

    /// A status read found a settled run in `from` to be `to` (a VM stopped outside the runner:
    /// failed; everything gone: absent). A move to `failed` is `not_ready`.
    ///
    /// # Errors
    ///
    /// The move isn't one an observation makes (`from` is moving, or the pair isn't in the table).
    pub fn observed(from: RunState, to: RunState) -> Result<Transition, IllegalTransition> {
        if !observable(from, to) {
            return Err(IllegalTransition {
                from,
                what: "be moved there by an observation",
            });
        }
        Ok(Transition {
            from,
            to,
            cause: (to == RunState::Failed).then_some(ErrorCode::NotReady),
            by: Cause::Observation,
        })
    }

    /// A deployment made outside the runner is taken over as a run that is up, from a run an
    /// earlier `down` left absent.
    ///
    /// # Errors
    ///
    /// `from` isn't `absent`: something of the run is there already.
    pub fn adopted(from: RunState) -> Result<Transition, IllegalTransition> {
        if from != RunState::Absent {
            return Err(IllegalTransition {
                from,
                what: "take a deployment over",
            });
        }
        Ok(Transition {
            from,
            to: RunState::Ready,
            cause: None,
            by: Cause::Observation,
        })
    }

    /// Where the run stood.
    #[must_use]
    pub fn from(&self) -> RunState {
        self.from
    }

    /// Where it stands after.
    #[must_use]
    pub fn to(&self) -> RunState {
        self.to
    }

    /// Why it failed, for a move to `failed`.
    #[must_use]
    pub fn cause(&self) -> Option<ErrorCode> {
        self.cause
    }

    /// What moved it.
    #[must_use]
    pub fn by(&self) -> Cause {
        self.by
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OP: OperationId = OperationId(7);

    fn every_state() -> Vec<RunState> {
        crate::testing::every_variant::<RunState>()
    }

    #[test]
    fn an_illegal_transition_is_refused() {
        for (from, op) in [
            (RunState::Parked, Op::Park),
            (RunState::Ready, Op::Resume),
            (RunState::Absent, Op::Update),
            (RunState::Creating, Op::Up),
            (RunState::Creating, Op::Down),
            (RunState::Ready, Op::Check),
        ] {
            let refused = Transition::begin(from, op, OP).unwrap_err();
            assert_eq!(refused.from, from, "{op}");
        }
        assert!(Transition::cut_off(RunState::Ready, OP).is_err(), "a settled run isn't recovered");
        assert!(
            Transition::observed(RunState::Creating, RunState::Ready).is_err(),
            "an observation never moves a moving run"
        );
        assert!(Transition::observed(RunState::Absent, RunState::Ready).is_err());
        assert_eq!(
            Transition::begin(RunState::Parked, Op::Park, OP).unwrap_err().to_string(),
            "a run parked can't be parked"
        );
    }

    #[test]
    fn each_op_moves_through_its_state_to_its_goal_or_to_failed() {
        let cases = [
            (RunState::Absent, Op::Up, RunState::Creating, RunState::Ready),
            (RunState::Failed, Op::Up, RunState::Creating, RunState::Ready),
            (RunState::Parked, Op::Resume, RunState::Resuming, RunState::Ready),
            (RunState::Ready, Op::Park, RunState::Parking, RunState::Parked),
            (RunState::Ready, Op::Update, RunState::Updating, RunState::Ready),
            (RunState::Parked, Op::Down, RunState::Destroying, RunState::Absent),
        ];
        for (from, op, during, goal) in cases {
            let begun = Transition::begin(from, op, OP).unwrap();
            assert_eq!((begun.from(), begun.to()), (from, during), "{op}");
            assert_eq!(begun.by(), Cause::Operation { id: OP, op });
            let ok = Transition::succeed(begun);
            assert_eq!((ok.from(), ok.to(), ok.cause()), (during, goal, None), "{op}");
            let failed = Transition::fail(begun, ErrorCode::Timeout);
            assert_eq!((failed.to(), failed.cause()), (RunState::Failed, Some(ErrorCode::Timeout)), "{op}");
        }
    }

    #[test]
    fn every_moving_state_is_recovered_to_failed_cancelled_and_nothing_else_is() {
        for from in every_state() {
            match Transition::cut_off(from, OP) {
                Ok(t) => {
                    assert!(from.is_moving());
                    assert_eq!(
                        (t.to(), t.cause(), t.by()),
                        (RunState::Failed, Some(ErrorCode::Cancelled), Cause::Recovery { id: OP })
                    );
                }
                Err(_) => assert!(!from.is_moving(), "{from}"),
            }
        }
    }

    #[test]
    fn the_wish_after_each_op_is_met_by_its_goal() {
        assert_eq!(Desired::after(Op::Up).map(Desired::met_by), Some(RunState::Ready));
        assert_eq!(Desired::after(Op::Park).map(Desired::met_by), Some(RunState::Parked));
        assert_eq!(Desired::after(Op::Down).map(Desired::met_by), Some(RunState::Absent));
        assert_eq!(Desired::after(Op::Check), None);
        crate::testing::assert_named_as_serde(Desired::as_str);
    }
}
