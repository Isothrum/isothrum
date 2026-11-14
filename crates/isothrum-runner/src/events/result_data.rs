//! What an operation's `result` event carries: one shape per operation, typed, so an embedder
//! reads `data` by its fields rather than walking untyped JSON. The JSON is the same as each
//! operation wrote by hand before: the enum is untagged, each shape told apart by its fields (the
//! embedder knows which operation it asked for).

use isoloom_manifest::OutputRole;
use serde::{Deserialize, Serialize};

use crate::state::RunState;

/// What an operation found or produced, by operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(untagged, deny_unknown_fields)]
#[non_exhaustive]
pub enum ResultData {
    /// An `up` that came up: where the run stands, its outputs, and its message.
    Started {
        /// Where the run stands (`ready`).
        state: RunState,
        /// Its outputs, sensitive values shown as `***`.
        outputs: Vec<ShownOutput>,
        /// Its message to the user, its placeholders filled, when the manifest gives one.
        message: Option<String>,
    },
    /// A `check`: how its checks went.
    Checked {
        /// How many passed.
        passed: usize,
        /// How many failed.
        failed: usize,
        /// The check runners that didn't finish (no `END` line).
        unfinished: Vec<String>,
    },
    /// A `reap`: the expired runs it destroyed or would, and what a cloud still holds.
    Swept {
        /// The cloud swept, by its wire name.
        cloud: String,
        /// The runs destroyed.
        destroyed: Vec<String>,
        /// The runs a dry run would destroy.
        pending: Vec<String>,
        /// The runs whose destroy failed, with why.
        failed: Vec<SweepFailure>,
        /// The cloud's resources past their expiry that no run records; none when the cloud
        /// couldn't be searched.
        orphans: Option<Vec<SweptResource>>,
        /// What of the cloud couldn't be read; none when it couldn't be searched.
        unread: Option<Vec<String>>,
    },
    /// A `detach`: the access machine taken out.
    Detached {
        /// Its manifest name.
        machine: String,
        /// Its native name (the Compose service / Vagrant define).
        native: String,
    },
    /// An `attach`: the access machine brought up, and where to reach it.
    Attached {
        /// Its manifest name.
        machine: String,
        /// Its native name.
        native: String,
        /// The run's network it is on.
        network: Option<String>,
        /// Its address on that network.
        address: Option<String>,
        /// The image or box it was brought up with, when the user picked one.
        image: Option<String>,
    },
    /// An operation on one machine or on all (`retire`, `provision`): which.
    Machine {
        /// Its manifest name; none for every machine (a `provision` of the whole run).
        machine: Option<String>,
    },
    /// An operation that moved the run (`down`, `park`, `resume`): where it stands.
    Moved {
        /// Where the run stands now.
        state: RunState,
    },
}

/// An output as a `result` event shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct ShownOutput {
    /// Its name.
    pub name: String,
    /// What it is for.
    pub role: OutputRole,
    /// Its value, when it can be read; `***` when it is sensitive.
    pub value: Option<String>,
}

impl ShownOutput {
    /// `name`, for `role`, showing `value`.
    #[must_use]
    pub fn new(name: impl Into<String>, role: OutputRole, value: Option<String>) -> ShownOutput {
        ShownOutput {
            name: name.into(),
            role,
            value,
        }
    }
}

/// A run a `reap` couldn't destroy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SweepFailure {
    /// The run.
    pub run: String,
    /// Why its destroy failed.
    pub error: String,
}

impl SweepFailure {
    /// Run `run`'s destroy, failed for `error`.
    #[must_use]
    pub fn new(run: impl Into<String>, error: impl Into<String>) -> SweepFailure {
        SweepFailure {
            run: run.into(),
            error: error.into(),
        }
    }
}

/// A cloud resource past its expiry that no run records, as a `reap` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct SweptResource {
    /// Its id as the cloud names it.
    pub id: String,
    /// What it is.
    pub kind: String,
    /// When its environment ends, in seconds since the Unix epoch, when it says.
    pub expires_at: Option<u64>,
    /// Its environment's name, when it says.
    pub instance: Option<String>,
}

impl SweptResource {
    /// Resource `id`, a `kind`, its environment `instance` ending at `expires_at`, as they say.
    #[must_use]
    pub fn new(id: impl Into<String>, kind: impl Into<String>, expires_at: Option<u64>, instance: Option<String>) -> SweptResource {
        SweptResource {
            id: id.into(),
            kind: kind.into(),
            expires_at,
            instance,
        }
    }
}

impl ResultData {
    /// The data with every string `mask` returns for it: no sensitive value of the run shows.
    #[must_use]
    pub(crate) fn masked(self, mask: impl Fn(&str) -> String) -> ResultData {
        let m = |s: String| mask(&s);
        let mo = |s: Option<String>| s.map(|s| mask(&s));
        match self {
            ResultData::Started { state, outputs, message } => ResultData::Started {
                state,
                outputs: outputs
                    .into_iter()
                    .map(|o| ShownOutput {
                        name: m(o.name),
                        role: o.role,
                        value: mo(o.value),
                    })
                    .collect(),
                message: mo(message),
            },
            ResultData::Attached {
                machine,
                native,
                network,
                address,
                image,
            } => ResultData::Attached {
                machine: m(machine),
                native: m(native),
                network: mo(network),
                address: mo(address),
                image: mo(image),
            },
            ResultData::Swept {
                cloud,
                destroyed,
                pending,
                failed,
                orphans,
                unread,
            } => ResultData::Swept {
                cloud,
                destroyed,
                pending,
                failed: failed.into_iter().map(|f| SweepFailure { run: f.run, error: m(f.error) }).collect(),
                orphans,
                unread,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each shape reads back as itself from the JSON it writes: its fields alone tell it apart.
    #[test]
    fn every_shape_reads_back_from_its_json() {
        let shapes = [
            ResultData::Started {
                state: RunState::Ready,
                outputs: vec![ShownOutput::new("web", OutputRole::Entry, Some("http://127.0.0.1:8080".into()))],
                message: None,
            },
            ResultData::Checked {
                passed: 2,
                failed: 0,
                unfinished: Vec::new(),
            },
            ResultData::Swept {
                cloud: "aws".into(),
                destroyed: vec!["a".into()],
                pending: Vec::new(),
                failed: vec![SweepFailure::new("b", "x")],
                orphans: None,
                unread: None,
            },
            ResultData::Detached {
                machine: "user".into(),
                native: "user".into(),
            },
            ResultData::Attached {
                machine: "user".into(),
                native: "user".into(),
                network: None,
                address: None,
                image: None,
            },
            ResultData::Machine { machine: Some("web".into()) },
            ResultData::Machine { machine: None },
            ResultData::Moved { state: RunState::Absent },
        ];
        for shape in shapes {
            let json = serde_json::to_string(&shape).unwrap();
            assert_eq!(serde_json::from_str::<ResultData>(&json).unwrap(), shape, "{json}");
        }
    }

    /// The JSON is the one each operation wrote by hand before the type.
    #[test]
    fn the_wire_shapes_are_unchanged() {
        let up = ResultData::Started {
            state: RunState::Ready,
            outputs: Vec::new(),
            message: Some("hi".into()),
        };
        assert_eq!(
            serde_json::to_value(&up).unwrap(),
            serde_json::json!({ "state": "ready", "outputs": [], "message": "hi" })
        );
        let moved = ResultData::Moved { state: RunState::Parked };
        assert_eq!(serde_json::to_value(&moved).unwrap(), serde_json::json!({ "state": "parked" }));
    }
}
