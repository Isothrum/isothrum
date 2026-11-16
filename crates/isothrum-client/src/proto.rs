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
//!
//! # What every protocol keeps
//!
//! A client of one release meets services of others: an app that bundles Isothrum finds the
//! service an older (or newer) release started. So that it can always tell which service runs and
//! replace it, every protocol, this one and every later one, keeps these two frames as they are:
//!
//! - `{"id":N,"cmd":"hello","protocol":P}` is answered `{"reply":"hello","id":N,"protocol":Q,
//!   "version":"…"}` whatever `P` is: `Q` is the protocol the service speaks, `version` its
//!   release. When `P` isn't `Q`, the connection then takes only `shutdown_if_idle` (any other
//!   request is refused).
//! - `{"id":N,"cmd":"shutdown_if_idle"}` is answered `{"reply":"ok","id":N}` (the service stops,
//!   and takes no operation meanwhile) or `{"reply":"busy","id":N,"operations":[…]}` (it has
//!   operations queued or running, and goes on).

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use isothrum_runner::{Format, InputVariable, Op, Park, Provider, RunName, Status};
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
    /// Bring the running run up to date with its project.
    Update {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
    },
    /// Bring a machine of the project that isn't running into the running run.
    Attach {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// The machine.
        machine: String,
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
        /// Only the provisioner of this name (Vagrant's `--provision-with`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        with: Option<String>,
    },
    /// Runs a command in a machine of the run, with no terminal; its result says how it ended.
    Exec {
        /// The run.
        target: Target,
        /// The values it runs with.
        #[serde(default)]
        options: OpOptions,
        /// The machine.
        machine: String,
        /// The command and its arguments.
        argv: Vec<String>,
    },
    /// The run's status (not queued: it reads). `target` is needed for a run not recorded yet;
    /// without it the run is reopened as recorded.
    Status {
        /// The run.
        run: RunName,
        /// Where it is opened from (default: where its record says).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<Target>,
        /// The values and secrets its read gives the tools, beside the recorded ones (a
        /// server's API token, for its machines' states there).
        #[serde(default, skip_serializing_if = "OpOptions::is_empty")]
        options: OpOptions,
    },
    /// The input variables a Terraform folder declares (not queued: it reads the folder's
    /// files, and runs nothing).
    Variables {
        /// The folder (its `name` is unused).
        target: Target,
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
    /// Stops the service only when no operation is queued or running ([`Reply::Ok`]: it takes no
    /// new one, and exits); else refused with [`Reply::Busy`], and nothing changes. Every protocol
    /// takes it, from a client of any protocol (see the module's documentation).
    ShutdownIfIdle,
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
            Command::Retire { .. } => Op::Retire,
            Command::Provision { .. } => Op::Provision,
            Command::Exec { .. } => Op::Check,
            Command::Hello { .. }
            | Command::Status { .. }
            | Command::Variables { .. }
            | Command::List
            | Command::Operations { .. }
            | Command::Cancel { .. }
            | Command::Subscribe { .. }
            | Command::Unsubscribe
            | Command::Shutdown
            | Command::ShutdownIfIdle => return None,
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
            | Command::Retire { target, options, .. }
            | Command::Provision { target, options, .. }
            | Command::Exec { target, options, .. } => Some((target, options)),
            _ => None,
        }
    }
}

/// Which run: its project's folder, absolute (the service doesn't share the client's working
/// folder), the name to run it under (default: the folder's), and its format when the folder
/// holds several (default: the one the runner recognizes there).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The project's folder (a file in it means the folder).
    pub path: PathBuf,
    /// The run's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<RunName>,
    /// The project's format, for a folder that holds several.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Format>,
}

impl Target {
    /// The project at `path`, under `name`, its format recognized.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, name: Option<RunName>) -> Target {
        Target {
            path: path.into(),
            name,
            format: None,
        }
    }

    /// The same, as a project of `format`.
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Target {
        self.format = Some(format);
        self
    }
}

/// The runner's options of an operation, on the wire. Secret values travel only over the owner-only endpoint and are
/// never stored or logged.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpOptions {
    /// The Vagrant provider, for a Vagrant project (a run keeps the one it started with).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<Provider>,
    /// Values every tool of the operation gets, by name, as its format reads them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Sensitive values, by name: as `env`, but masked in every event and never stored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub secrets: BTreeMap<String, String>,
    /// What to call the place the run is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_label: Option<String>,
    /// A status read only: each sensitive output's own value instead of `***`. The service gives
    /// it only over its owner-only endpoint; it is never in an event, never stored. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reveal: bool,
}

impl OpOptions {
    /// Whether it gives nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == OpOptions::default()
    }
}

impl fmt::Debug for OpOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpOptions")
            .field("provider", &self.provider)
            .field("env", &self.env)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("host_label", &self.host_label)
            .field("reveal", &self.reveal)
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
    /// To `variables`.
    Variables {
        /// The request's id.
        id: u64,
        /// The folder's input variables, by name.
        variables: Vec<InputVariable>,
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
    /// Done (`subscribe`, `unsubscribe`, `cancel`, `shutdown`, `shutdown_if_idle`).
    Ok {
        /// The request's id.
        id: u64,
    },
    /// Refused, or failed.
    Error {
        /// The request's id (0 for a line that couldn't be read as a request).
        id: u64,
        /// Why: its code, message and typed fields, as the runner reports a failure.
        #[serde(flatten)]
        report: isothrum_runner::ErrorReport,
    },
    /// To `shutdown_if_idle`, refused: the operations queued or running, which the service goes on
    /// with.
    Busy {
        /// The request's id.
        id: u64,
        /// The operations, oldest first.
        operations: Vec<OperationId>,
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
            | Reply::Variables { id, .. }
            | Reply::Operations { id, .. }
            | Reply::Ok { id }
            | Reply::Busy { id, .. }
            | Reply::Error { id, .. } => Some(*id),
            Reply::Event { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    /// `value` written as one line, read back as itself: the line, as JSON.
    fn round_trip<T: Serialize + serde::de::DeserializeOwned + PartialEq + fmt::Debug>(value: &T) -> serde_json::Value {
        let line = serde_json::to_string(value).unwrap();
        assert!(!line.contains('\n'), "one line: {line}");
        assert_eq!(&serde_json::from_str::<T>(&line).unwrap(), value, "{line}");
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn a_request_is_one_flat_json_object() {
        let up = Request {
            id: 7,
            command: Command::Up {
                target: Target::new("/labs/web", Some(RunName::new("web").unwrap())).with_format(Format::Compose),
                options: OpOptions::default(),
                lifetime_s: Some(3600),
            },
        };
        assert_eq!(
            round_trip(&up),
            json!({"id": 7, "cmd": "up", "target": {"path": "/labs/web", "name": "web", "format": "compose"}, "options": {}, "lifetime_s": 3600})
        );
        let park = round_trip(&Request {
            id: 8,
            command: Command::Park {
                target: Target::new("/labs/web", None),
                options: OpOptions::default(),
                mode: Park::Pause,
            },
        });
        assert_eq!((&park["cmd"], &park["mode"]), (&json!("park"), &json!("pause")), "{park}");
        round_trip(&Request { id: 9, command: Command::List });
        assert_eq!(
            round_trip(&Request {
                id: 10,
                command: Command::Subscribe {
                    run: None,
                    after: Some(EventCursor(41)),
                },
            }),
            json!({"id": 10, "cmd": "subscribe", "after": 41})
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
    fn reveal_is_off_unless_asked_and_travels_only_when_on() {
        assert_eq!(serde_json::to_string(&OpOptions::default()).unwrap(), "{}");
        let on = OpOptions {
            reveal: true,
            ..OpOptions::default()
        };
        let json = serde_json::to_string(&on).unwrap();
        assert_eq!(json, r#"{"reveal":true}"#);
        assert!(serde_json::from_str::<OpOptions>(&json).unwrap().reveal);
        assert!(!serde_json::from_str::<OpOptions>("{}").unwrap().reveal, "off when not said");
    }

    #[test]
    fn replies_round_trip() {
        let accepted = round_trip(&Reply::Accepted {
            id: 1,
            operation: OperationId(4),
            run: RunName::new("web").unwrap(),
            position: 1,
        });
        assert_eq!(accepted, json!({"reply": "accepted", "id": 1, "operation": 4, "run": "web", "position": 1}));
        let refused = round_trip(&Reply::Error {
            id: 2,
            report: isothrum_runner::ErrorReport::new(isothrum_runner::ErrorCode::Usage, "no").unchanged(),
        });
        assert_eq!(refused, json!({"reply": "error", "id": 2, "code": "usage", "message": "no", "changed": false}));
        let run = RunName::new("web").unwrap();
        let event = crate::v1::phase_event(&run, (Op::Down, OperationId(4)), crate::v1::OperationPhase::Started, None, None);
        let frame = round_trip(&Reply::Event {
            cursor: EventCursor(12),
            event: Box::new(event),
        });
        assert_eq!(
            (&frame["reply"], &frame["cursor"], &frame["event"]["v"]),
            (&json!("event"), &json!(12), &json!(1)),
            "{frame}"
        );
    }

    #[test]
    fn the_frames_every_protocol_keeps_read_as_written() {
        let hello = r#"{"id":0,"cmd":"hello","protocol":7}"#;
        assert_eq!(serde_json::from_str::<Request>(hello).unwrap().command, Command::Hello { protocol: 7 });
        assert_eq!(
            round_trip(&Request {
                id: 3,
                command: Command::ShutdownIfIdle,
            }),
            json!({"id": 3, "cmd": "shutdown_if_idle"})
        );
        let hello_reply = round_trip(&Reply::Hello {
            id: 0,
            protocol: 7,
            version: "9.0.0".into(),
        });
        assert_eq!(hello_reply, json!({"reply": "hello", "id": 0, "protocol": 7, "version": "9.0.0"}));
        let busy = round_trip(&Reply::Busy {
            id: 3,
            operations: vec![OperationId(4), OperationId(9)],
        });
        assert_eq!(busy, json!({"reply": "busy", "id": 3, "operations": [4, 9]}));
        assert_eq!(round_trip(&Reply::Ok { id: 3 }), json!({"reply": "ok", "id": 3}));
    }
}
