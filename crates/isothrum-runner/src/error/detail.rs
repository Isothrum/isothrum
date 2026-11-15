//! What a failure says besides its code and message, as typed fields an embedder acts on (the
//! port to free, the tool to install and the version it needs, how long a download stalled, the
//! server that refused a sign-in, the secret to give). Versioned ([`DETAIL_VERSION`]), so a field
//! a later runner adds is a new version, never a changed meaning.
//!
//! Never a secret, and never free text a tool printed (which may echo one): those stay in the
//! message, which the event stream masks.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Error, TimedOut};
use crate::state::RunState;

/// The version of [`ErrorDetail`]'s fields.
pub const DETAIL_VERSION: u32 = 1;

/// A failure's typed fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ErrorDetail {
    /// The version of the fields ([`DETAIL_VERSION`]).
    pub v: u32,
    /// The fields, by the failure's kind.
    #[serde(flatten)]
    pub data: Detail,
}

/// A failure's typed fields, by its kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Detail {
    /// A host port the run publishes is taken.
    PortInUse {
        /// The port.
        port: u16,
        /// `tcp` or `udp`.
        protocol: String,
    },
    /// A tool isn't installed.
    ToolMissing {
        /// The tool.
        tool: String,
    },
    /// A tool is older than the run needs.
    ToolTooOld {
        /// The tool.
        tool: String,
        /// The version it answered with.
        found: String,
        /// The oldest version the run takes.
        required: String,
    },
    /// A tool's service didn't answer (Docker's engine).
    ToolUnanswered {
        /// The tool.
        tool: String,
    },
    /// A step ran out of time.
    Timeout {
        /// Which limit it ran into.
        timed_out: TimedOut,
    },
    /// A download showed no progress for as long as a download may.
    DownloadStalled {
        /// How long it went with no progress, in seconds.
        after_s: u64,
    },
    /// The run is recorded on another place than the one asked for.
    RunningElsewhere {
        /// Where it is recorded.
        recorded: String,
        /// Where it was asked for.
        asked: String,
    },
    /// The run can't be parked in that mode.
    NotParkable {
        /// The mode.
        mode: String,
    },
    /// Machines of the run are gone, so it can't be resumed whole.
    PartlyGone {
        /// The machines gone.
        machines: Vec<String>,
    },
    /// A secret the run needs has no value.
    SecretMissing {
        /// Its name.
        name: String,
    },
    /// A value the run was given can't be used (its name only: the value may be a secret).
    InputInvalid {
        /// Its name.
        name: String,
    },
    /// The folder isn't a project to run.
    ProjectInvalid {
        /// The folder.
        dir: PathBuf,
    },
    /// A machine the run doesn't have.
    MachineUnknown {
        /// The machine asked for.
        machine: String,
    },
    /// A network the run doesn't have.
    NetworkUnknown {
        /// The network asked for.
        network: String,
    },
    /// A Vagrant provider refused.
    ProviderRefused {
        /// The provider.
        provider: String,
    },
    /// A machine has no graphical console to reach.
    NoConsole {
        /// The machine.
        machine: String,
    },
    /// The server the run is on refused its credentials.
    ServerAuthFailed {
        /// The server, when known.
        server: Option<String>,
    },
    /// A file or folder couldn't be read or written.
    Io {
        /// The path.
        path: PathBuf,
        /// What was being done (`read`, `create`).
        action: String,
    },
    /// The run moved between a read and the write that would have moved it.
    StateConflict {
        /// Where the write expected it.
        expected: RunState,
        /// Where it was.
        found: Option<RunState>,
    },
    /// A live process holds the lease asked for.
    LeaseHeld {
        /// What the lease covers.
        scope: String,
        /// The holder's process id.
        pid: u32,
    },
}

impl Error {
    /// The failure's typed fields, for one that has any beyond its code and message.
    #[must_use]
    pub fn detail(&self) -> Option<ErrorDetail> {
        let data = match self {
            Error::PortInUse { port, protocol } => Detail::PortInUse {
                port: port.get(),
                protocol: protocol.clone(),
            },
            Error::ToolMissing { tool, .. } => Detail::ToolMissing { tool: tool.clone() },
            Error::ToolTooOld { tool, found, required } => Detail::ToolTooOld {
                tool: tool.clone(),
                found: found.clone(),
                required: required.clone(),
            },
            Error::ToolUnanswered { tool, .. } => Detail::ToolUnanswered { tool: tool.clone() },
            Error::Timeout { timed_out, .. } => Detail::Timeout {
                // Its last line is a tool's: kept out, as any text a tool printed.
                timed_out: match timed_out {
                    TimedOut::NoProgress { after_s, .. } => TimedOut::NoProgress {
                        after_s: *after_s,
                        last_line: None,
                    },
                    TimedOut::Ceiling { after_s, .. } => TimedOut::Ceiling {
                        after_s: *after_s,
                        last_line: None,
                    },
                },
            },
            Error::DownloadStalled { after_s, .. } => Detail::DownloadStalled { after_s: *after_s },
            Error::RunningElsewhere { recorded, asked, .. } => Detail::RunningElsewhere {
                recorded: recorded.clone(),
                asked: asked.clone(),
            },
            Error::NotParkable { mode, .. } => Detail::NotParkable { mode: (*mode).to_owned() },
            Error::PartlyGone { machines, .. } => Detail::PartlyGone { machines: machines.clone() },
            Error::SecretMissing { name } => Detail::SecretMissing { name: name.clone() },
            Error::Input { name, .. } => Detail::InputInvalid { name: name.clone() },
            Error::Project { dir, .. } => Detail::ProjectInvalid { dir: dir.clone() },
            Error::MachineUnknown { machine, .. } => Detail::MachineUnknown { machine: machine.clone() },
            Error::NetworkUnknown { network, .. } => Detail::NetworkUnknown { network: network.clone() },
            Error::Provider { provider, .. } => Detail::ProviderRefused { provider: provider.clone() },
            Error::NoConsole { machine, .. } => Detail::NoConsole { machine: machine.clone() },
            Error::ServerAuthFailed { server, .. } => Detail::ServerAuthFailed { server: server.clone() },
            Error::Io { path, action, .. } => Detail::Io {
                path: path.clone(),
                action: (*action).to_owned(),
            },
            Error::StateConflict { expected, found, .. } => Detail::StateConflict {
                expected: *expected,
                found: *found,
            },
            Error::LeaseHeld { scope, pid } => Detail::LeaseHeld {
                scope: scope.clone(),
                pid: *pid,
            },
            _ => return None,
        };
        Some(ErrorDetail { v: DETAIL_VERSION, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_port_in_use_says_its_port_versioned_and_flat() {
        let e = Error::PortInUse {
            port: crate::port::HostPort::new(8080),
            protocol: "tcp".to_owned(),
        };
        let json = serde_json::to_value(e.detail().expect("a detail")).expect("json");
        assert_eq!(json, serde_json::json!({ "v": 1, "kind": "port_in_use", "port": 8080, "protocol": "tcp" }));
    }

    #[test]
    fn a_timeout_keeps_no_line_a_tool_printed() {
        let e = Error::Timeout {
            step: "up".to_owned(),
            timed_out: TimedOut::NoProgress {
                after_s: 30,
                last_line: Some("token=s3cret".to_owned()),
            },
        };
        let json = serde_json::to_string(&e.detail().expect("a detail")).expect("json");
        assert!(!json.contains("s3cret"), "{json}");
    }

    #[test]
    fn a_failure_with_only_a_reason_has_no_detail() {
        let e = Error::ToolFailed {
            step: "up".to_owned(),
            reason: "boom".to_owned(),
        };
        assert!(e.detail().is_none());
    }
}
