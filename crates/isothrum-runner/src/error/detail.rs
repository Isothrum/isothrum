//! What a failure says besides its code and message, as typed fields an embedder acts on (the
//! port to free, the tool to install and the version it needs, how long a download stalled, the
//! server that refused a sign-in, the secret to give). Versioned ([`DETAIL_VERSION`]), so a field
//! a later runner adds is a new version, never a changed meaning.
//!
//! Never a secret. What a tool printed (a download's reason, a timed-out step's last line) is
//! masked as the message is, wherever the event stream masks it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Error, TimedOut};
use crate::state::RunState;

/// The version of [`ErrorDetail`]'s fields.
pub const DETAIL_VERSION: u32 = 2;

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

/// What a download fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DownloadKind {
    /// Container images (a Compose start's pull).
    Images,
    /// Vagrant boxes (what a Vagrant start boots).
    Boxes,
}

/// What a computer lacks for a Vagrant provider to run a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderNeed {
    /// virtiofsd, which libvirt and QEMU start to share a host folder with a VM through virtiofs
    /// (Linux): none is installed where they look.
    Virtiofsd,
    /// A permission of the operating system's: the hypervisor's app, driven through Apple events
    /// (UTM, by `vagrant_utm`), refused to act with the system's `Operation not permitted`, or
    /// the program driving it may not send it Apple events (`-1743`, macOS's Automation
    /// permission). Waiting doesn't fix it; the user's settings, or the hypervisor's, do.
    Permission,
}

/// The process that holds a host port: what a `port_in_use` names so the user knows what to stop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct PortHolder {
    /// Its process id.
    pub pid: u32,
    /// Its program's name, as the system lists it (`nginx`, `com.docke`: `lsof` cuts it to 9
    /// characters).
    pub program: String,
}

impl PortHolder {
    /// Process `pid` running `program`.
    #[must_use]
    pub fn new(pid: u32, program: impl Into<String>) -> PortHolder {
        PortHolder { pid, program: program.into() }
    }
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
        /// The process listening on it, when it was found.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        holder: Option<PortHolder>,
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
        /// What it downloads.
        what: DownloadKind,
        /// How long it went with no progress, in seconds.
        after_s: u64,
    },
    /// A download couldn't be made.
    DownloadFailed {
        /// What it downloads.
        what: DownloadKind,
        /// The last try's last error lines, or its exit status (masked).
        reason: String,
    },
    /// The elevation the run's commands need couldn't be had.
    Elevation {
        /// Why (the prompt declined, the elevated session ended).
        reason: String,
    },
    /// A ready condition didn't hold.
    NotReady {
        /// The condition (`every machine of the run`, `tcp 127.0.0.1:8080`).
        condition: String,
        /// Why it didn't.
        detail: String,
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
    /// A value the run was given can't be used.
    InputInvalid {
        /// Its name.
        name: String,
        /// Why (masked: it may quote the value).
        reason: String,
    },
    /// A step's tool ran and failed.
    ToolFailed {
        /// Its last error lines, or its exit status (masked).
        reason: String,
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
        /// What this computer lacks for it, when that is what refused it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        need: Option<ProviderNeed>,
    },
    /// A tool refused this account.
    ToolDenied {
        /// The tool.
        tool: String,
    },
    /// A cloud refused the sign-in the run's tools use.
    CredentialsExpired {
        /// The cloud, when the failure names its Terraform provider.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cloud: Option<crate::project::platform::Cloud>,
        /// What the tool said (masked).
        reason: String,
    },
    /// A network the run makes needs an address block another network holds.
    AddressPoolOverlap {
        /// What the tool said (masked).
        reason: String,
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
        let data = self.tool_detail().or_else(|| self.run_detail())?;
        Some(ErrorDetail { v: DETAIL_VERSION, data })
    }

    /// The fields of a failure of a tool, a download or a step.
    fn tool_detail(&self) -> Option<Detail> {
        Some(match self {
            Error::PortInUse { port, protocol, holder } => Detail::PortInUse {
                port: port.get(),
                protocol: protocol.clone(),
                holder: holder.clone(),
            },
            Error::ToolMissing { tool, .. } => Detail::ToolMissing { tool: tool.clone() },
            Error::ToolTooOld { tool, found, required } => Detail::ToolTooOld {
                tool: tool.clone(),
                found: found.clone(),
                required: required.clone(),
            },
            Error::ToolUnanswered { tool, .. } => Detail::ToolUnanswered { tool: tool.clone() },
            Error::Timeout { timed_out, .. } => Detail::Timeout { timed_out: timed_out.clone() },
            Error::DownloadStalled { what, after_s, .. } => Detail::DownloadStalled {
                what: *what,
                after_s: *after_s,
            },
            Error::DownloadFailed { what, reason, .. } => Detail::DownloadFailed {
                what: *what,
                reason: reason.clone(),
            },
            Error::Elevation { reason } => Detail::Elevation { reason: reason.clone() },
            Error::NotReady { condition, detail } => Detail::NotReady {
                condition: condition.clone(),
                detail: detail.clone(),
            },
            Error::ToolFailed { reason, .. } => Detail::ToolFailed { reason: reason.clone() },
            Error::ToolDenied { tool, .. } => Detail::ToolDenied { tool: tool.clone() },
            Error::CredentialsExpired { cloud, reason, .. } => Detail::CredentialsExpired {
                cloud: *cloud,
                reason: reason.clone(),
            },
            Error::AddressPoolOverlap { reason, .. } => Detail::AddressPoolOverlap { reason: reason.clone() },
            _ => return None,
        })
    }

    /// The fields of a failure of the run, its inputs or the home.
    fn run_detail(&self) -> Option<Detail> {
        Some(match self {
            Error::RunningElsewhere { recorded, asked, .. } => Detail::RunningElsewhere {
                recorded: recorded.clone(),
                asked: asked.clone(),
            },
            Error::NotParkable { mode, .. } => Detail::NotParkable { mode: (*mode).to_owned() },
            Error::PartlyGone { machines, .. } => Detail::PartlyGone { machines: machines.clone() },
            Error::SecretMissing { name } => Detail::SecretMissing { name: name.clone() },
            Error::Input { name, reason } => Detail::InputInvalid {
                name: name.clone(),
                reason: reason.clone(),
            },
            Error::Project { dir, .. } => Detail::ProjectInvalid { dir: dir.clone() },
            Error::MachineUnknown { machine, .. } => Detail::MachineUnknown { machine: machine.clone() },
            Error::NetworkUnknown { network, .. } => Detail::NetworkUnknown { network: network.clone() },
            Error::Provider { provider, need, .. } => Detail::ProviderRefused {
                provider: provider.clone(),
                need: *need,
            },
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
        })
    }
}

impl ErrorDetail {
    /// The same, the text a tool printed passed through `mask`.
    pub(crate) fn masked(self, mask: impl Fn(&str) -> String) -> ErrorDetail {
        let data = match self.data {
            Detail::Timeout { timed_out } => Detail::Timeout {
                timed_out: timed_out.masked(&mask),
            },
            Detail::DownloadFailed { what, reason } => Detail::DownloadFailed { what, reason: mask(&reason) },
            Detail::Elevation { reason } => Detail::Elevation { reason: mask(&reason) },
            Detail::ToolFailed { reason } => Detail::ToolFailed { reason: mask(&reason) },
            Detail::CredentialsExpired { cloud, reason } => Detail::CredentialsExpired { cloud, reason: mask(&reason) },
            Detail::AddressPoolOverlap { reason } => Detail::AddressPoolOverlap { reason: mask(&reason) },
            Detail::InputInvalid { name, reason } => Detail::InputInvalid { name, reason: mask(&reason) },
            Detail::NotReady { condition, detail } => Detail::NotReady {
                condition,
                detail: mask(&detail),
            },
            other => other,
        };
        ErrorDetail { data, ..self }
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
            holder: None,
        };
        let json = serde_json::to_value(e.detail().expect("a detail")).expect("json");
        assert_eq!(json, serde_json::json!({ "v": 2, "kind": "port_in_use", "port": 8080, "protocol": "tcp" }));
        let held = Error::PortInUse {
            port: crate::port::HostPort::new(8080),
            protocol: "tcp".to_owned(),
            holder: Some(PortHolder::new(4123, "nginx")),
        };
        let held_json = serde_json::to_value(held.detail().expect("a detail")).expect("json");
        assert_eq!(held_json["holder"], serde_json::json!({ "pid": 4123, "program": "nginx" }));
        assert!(held.to_string().ends_with(": `nginx` (pid 4123) listens on it"), "{held}");
    }

    #[test]
    fn what_a_tool_printed_in_the_fields_is_masked_as_the_message_is() {
        let timeout = Error::Timeout {
            step: "up".to_owned(),
            timed_out: TimedOut::NoProgress {
                after_s: 30,
                last_line: Some("token=s3cret".to_owned()),
            },
        };
        let failed = Error::DownloadFailed {
            what: DownloadKind::Images,
            step: "pull-up".to_owned(),
            reason: "denied for s3cret".to_owned(),
        };
        for e in [timeout, failed] {
            let detail = e.detail().expect("a detail");
            assert!(serde_json::to_string(&detail).expect("json").contains("s3cret"));
            let masked = serde_json::to_string(&detail.masked(|t| t.replace("s3cret", "***"))).expect("json");
            assert!(!masked.contains("s3cret"), "{masked}");
        }
    }

    #[test]
    fn a_download_says_what_it_fetches() {
        let e = Error::DownloadStalled {
            what: DownloadKind::Boxes,
            step: "up".to_owned(),
            after_s: 900,
            last_line: None,
        };
        let json = serde_json::to_value(e.detail().expect("a detail")).expect("json");
        assert_eq!(json, serde_json::json!({ "v": 2, "kind": "download_stalled", "what": "boxes", "after_s": 900 }));
    }

    #[test]
    fn a_failure_with_only_a_message_has_no_detail() {
        let e = Error::Locked { run: "r".to_owned() };
        assert!(e.detail().is_none());
    }
}
