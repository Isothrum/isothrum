//! The service's wire protocol, version [`PROTOCOL`]: newline-delimited JSON (one object per
//! line, UTF-8, at most [`MAX_LINE`] bytes) over the home's local endpoint.
//!
//! The client writes [`Request`]s, each with an id of its choosing; the service answers each with
//! one [`Reply`] carrying that id. The first request of a connection is [`Command::Hello`]; the
//! service refuses any other before it. An operation command (`up`, `down`, `park`, `resume`,
//! `update`, `attach`, `detach`, `retire`, `provision`, `check`) is queued on its run and
//! answered at once with [`Reply::Accepted`]; the operation's events, ending with its `result` or
//! `error` and an `operation` event of phase `ended`, arrive on a subscription: after `subscribe`,
//! the connection carries [`Reply::Event`] frames until it closes or `unsubscribe`.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use isothrum_runner::{ErrorCode, Op, Park, RunName, Status};
use serde::{Deserialize, Serialize};

use crate::v1::{Event, EventCursor, OperationId, OperationRow, RunRow};

/// The protocol's version: a service refuses a `hello` with another.
pub const PROTOCOL: u32 = 1;

/// The longest line either side accepts, in bytes (a longer one ends the connection).
pub const MAX_LINE: usize = 16 * 1024 * 1024;

/// One request: its id and what it asks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// The id its reply carries.
    pub id: u64,
    /// What it asks.
    #[serde(flatten)]
    pub command: Command,
}

/// What a request asks of the service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Command {
    /// Opens the conversation (must come first).
    Hello {
        /// The protocol the client speaks ([`PROTOCOL`]).
        protocol: u32,
    },
    /// Start the run (a parked run is resumed).
    Up {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// Remove whatever of the run exists, then start afresh.
        #[serde(default)]
        fresh: bool,
        /// How long a run that stops itself may live, in seconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lifetime_s: Option<u64>,
    },
    /// Remove everything the run created.
    Down {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
    },
    /// Stop the run, keeping what it created.
    Park {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// Paused or shut down.
        mode: Park,
    },
    /// Start a parked run again.
    Resume {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
    },
    /// Bring the running run up to date with its manifest.
    Update {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
    },
    /// Hot-add the on-demand access machine.
    Attach {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// Which on-demand machine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
        /// The network to attach it to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network: Option<String>,
        /// The image or box to run it from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    /// Hot-remove the on-demand access machine.
    Detach {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// Which on-demand machine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// Take a machine out of the running run.
    Retire {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// The machine.
        machine: String,
    },
    /// Run the set-up again on the running machines.
    Provision {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// Only this machine.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine: Option<String>,
    },
    /// Run the run's checks.
    Check {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
    },
    /// The run's status (not queued: it reads). `path` is needed for a run not recorded yet.
    Status {
        /// The run.
        run: RunName,
        /// Where it is opened from (default: where its record says).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<PathBuf>,
    },
    /// Every recorded run.
    List,
    /// The run's latest operations, newest first.
    Operations {
        /// The run.
        run: RunName,
        /// At most this many.
        limit: u32,
    },
    /// Cancels an operation: a queued one never begins, a running one is cut short (its tools
    /// stopped).
    Cancel {
        /// The operation.
        operation: OperationId,
    },
    /// Turns the connection into a subscription: the events after `after` (every one the
    /// service keeps when `None`), then the live ones, of one run or every run.
    Subscribe {
        /// Only this run's.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<RunName>,
        /// Resumes after this cursor (a reconnecting client's last).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after: Option<EventCursor>,
    },
    /// Ends the connection's subscription.
    Unsubscribe,
    /// Stops the service: queued operations are dropped, running ones cancelled and their tools
    /// stopped, then it exits.
    Shutdown,
}

impl Command {
    /// The operation it queues, when it queues one.
    #[must_use]
    pub fn op(&self) -> Option<Op> {
        Some(match self {
            Command::Up { .. } => Op::Up,
            Command::Down { .. } => Op::Down,
            Command::Park { .. } => Op::Park,
            Command::Resume { .. } => Op::Resume,
            Command::Update { .. } => Op::Update,
            Command::Attach { .. } => Op::Attach,
            Command::Detach { .. } => Op::Detach,
            Command::Retire { .. } => Op::Retire,
            Command::Provision { .. } => Op::Provision,
            Command::Check { .. } => Op::Check,
            Command::Hello { .. }
            | Command::Status { .. }
            | Command::List
            | Command::Operations { .. }
            | Command::Cancel { .. }
            | Command::Subscribe { .. }
            | Command::Unsubscribe
            | Command::Shutdown => return None,
        })
    }

    /// The run and the options of an operation command.
    #[must_use]
    pub fn target(&self) -> Option<(&Target, &OpOptions)> {
        match self {
            Command::Up { target, options, .. }
            | Command::Down { target, options }
            | Command::Park { target, options, .. }
            | Command::Resume { target, options }
            | Command::Update { target, options }
            | Command::Attach { target, options, .. }
            | Command::Detach { target, options, .. }
            | Command::Retire { target, options, .. }
            | Command::Provision { target, options, .. }
            | Command::Check { target, options } => Some((target, options)),
            _ => None,
        }
    }
}

/// Which run: the path the runner opens it from, absolute (the service doesn't share the
/// client's working folder), and the name to run it under (default: the one the runner reads
/// there). The protocol knows nothing of what is there: the runner recognizes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The run's folder (or file), as the runner opens it.
    pub path: PathBuf,
    /// The run's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<RunName>,
}

impl Target {
    /// The run at `path`, under `name`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, name: Option<RunName>) -> Target {
        Target { path: path.into(), name }
    }
}

/// The runner's options of an operation, on the wire. Secret values travel only over the owner-only endpoint and are
/// never stored or logged.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpOptions {
    /// The backend of a tool that has several, by the tool's own name for it (a run keeps the one
    /// it started with).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Params, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, String>,
    /// Secret and input slot values, by slot name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
    /// What to call the place the run is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// The login the run's SSH goes through first (read by an `up` that starts it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_jump: Option<String>,
}

impl fmt::Debug for OpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpOptions")
            .field("provider", &self.provider)
            .field("params", &self.params)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("host_label", &self.host_label)
            .field("ssh_jump", &self.ssh_jump)
            .finish()
    }
}

/// What the service writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Reply {
    /// To `hello`.
    Hello {
        /// The request's id.
        id: u64,
        /// The protocol it speaks.
        protocol: u32,
        /// Its version.
        version: String,
    },
    /// An operation command was queued: its id, and its place in the run's queue (0: it begins
    /// now).
    Accepted {
        /// The request's id.
        id: u64,
        /// The operation's id.
        operation: OperationId,
        /// Its run.
        run: RunName,
        /// How many of the run's operations are ahead of it.
        position: u32,
    },
    /// To `status`.
    Status {
        /// The request's id.
        id: u64,
        /// The status.
        status: Box<Status>,
    },
    /// To `list`.
    Runs {
        /// The request's id.
        id: u64,
        /// Every recorded run.
        runs: Vec<RunRow>,
    },
    /// To `operations`.
    Operations {
        /// The request's id.
        id: u64,
        /// The operations, newest first.
        operations: Vec<OperationRow>,
    },
    /// Done (`subscribe`, `unsubscribe`, `cancel`, `shutdown`).
    Ok {
        /// The request's id.
        id: u64,
    },
    /// Refused, or failed.
    Error {
        /// The request's id (0 for a line that couldn't be read as a request).
        id: u64,
        /// Why.
        code: ErrorCode,
        /// What went wrong.
        message: String,
    },
    /// A subscription's frame.
    Event {
        /// Its place in the stream.
        cursor: EventCursor,
        /// The event.
        event: Box<Event>,
    },
}

impl Reply {
    /// The request it answers; `None` for a subscription's frame.
    #[must_use]
    pub fn id(&self) -> Option<u64> {
        match self {
            Reply::Hello { id, .. }
            | Reply::Accepted { id, .. }
            | Reply::Status { id, .. }
            | Reply::Runs { id, .. }
            | Reply::Operations { id, .. }
            | Reply::Ok { id }
            | Reply::Error { id, .. } => Some(*id),
            Reply::Event { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + fmt::Debug>(value: &T) -> String {
        let line = serde_json::to_string(value).unwrap();
        assert_eq!(&serde_json::from_str::<T>(&line).unwrap(), value, "{line}");
        line
    }

    #[test]
    fn a_request_is_one_flat_json_object() {
        let up = Request {
            id: 7,
            command: Command::Up {
                target: Target::new("/runs/web/.isoloom/docker/run.json", Some(RunName::new("web").unwrap())),
                options: OpOptions::default(),
                fresh: true,
                lifetime_s: None,
            },
        };
        assert_eq!(
            round_trip(&up),
            r#"{"id":7,"cmd":"up","target":{"path":"/runs/web/.isoloom/docker/run.json","name":"web"},"options":{},"fresh":true}"#
        );
        let park = round_trip(&Request {
            id: 8,
            command: Command::Park {
                target: Target::new("/m.json", None),
                options: OpOptions::default(),
                mode: Park::Pause,
            },
        });
        assert!(park.contains(r#""cmd":"park""#) && park.contains(r#""mode":"pause""#), "{park}");
        round_trip(&Request { id: 9, command: Command::List });
        assert_eq!(
            round_trip(&Request {
                id: 10,
                command: Command::Subscribe {
                    run: None,
                    after: Some(EventCursor(41)),
                },
            }),
            r#"{"id":10,"cmd":"subscribe","after":41}"#
        );
    }

    #[test]
    fn a_secret_is_never_in_the_debug_form() {
        let options = OpOptions {
            secrets: [("TOKEN".to_owned(), "hunter2".to_owned())].into(),
            ..OpOptions::default()
        };
        let shown = format!("{options:?}");
        assert!(shown.contains("TOKEN") && !shown.contains("hunter2"), "{shown}");
    }

    #[test]
    fn replies_round_trip() {
        let accepted = round_trip(&Reply::Accepted {
            id: 1,
            operation: OperationId(4),
            run: RunName::new("web").unwrap(),
            position: 1,
        });
        assert_eq!(accepted, r#"{"reply":"accepted","id":1,"operation":4,"run":"web","position":1}"#);
        let refused = round_trip(&Reply::Error {
            id: 2,
            code: ErrorCode::Usage,
            message: "no".into(),
        });
        assert_eq!(refused, r#"{"reply":"error","id":2,"code":"usage","message":"no"}"#);
        let run = RunName::new("web").unwrap();
        let event = Event::phase(&run, Op::Down, OperationId(4), crate::v1::OperationPhase::Started, None);
        let frame = round_trip(&Reply::Event {
            cursor: EventCursor(12),
            event: Box::new(event),
        });
        assert!(frame.starts_with(r#"{"reply":"event","cursor":12,"event":{"v":1"#), "{frame}");
    }
}
