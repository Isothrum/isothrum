//! The stable code of each failure.

use serde::{Deserialize, Serialize};

/// The stable code of a failure, the same in the Rust API and the event stream (`error.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The manifest isn't valid JSON or breaks the contract, a value it gives can't be used as
    /// written (a step's `verdict` that isn't a regular expression, an SSH login, a `wait_s`, a
    /// `tcp` condition's port), or it names a value the operation doesn't have (a placeholder
    /// with none, a guest delivery's machine whose address no output gives, a Terraform module
    /// with no `.tf` file).
    ManifestInvalid,
    /// The manifest is another version of the contract.
    ManifestVersion,
    /// The manifest needs something this runner doesn't do (a target that doesn't run on this
    /// host, a delivery its driver doesn't take, an action of another driver's, a step with no
    /// command for this host), or the call asks the run for something its target can't do (a
    /// console into a Compose service with no VM, a Vagrant VM joined to another network).
    Unsupported,
    /// A file or folder couldn't be read or written, or a socket on this computer bound.
    Io,
    /// Another process runs an operation on this run, or holds the home's slot pick.
    Locked,
    /// A tool a step runs isn't installed.
    ToolMissing,
    /// A step's tool ran and failed.
    ToolFailed,
    /// A tool answers at a version older than the run needs.
    ToolTooOld,
    /// A step showed no progress for its time limit, or ran to the ceiling every step stops at,
    /// and was stopped, with what it started (the `error` event's `timeout` says which).
    Timeout,
    /// A download (what a step needs, fetched in a phase of its own before it: a Compose start's
    /// images, a Vagrant start's boxes; or a step the manifest marks a download) showed no
    /// progress for as long as a download may, and was stopped.
    DownloadStalled,
    /// A download couldn't be made: its tool failed (the message says why: a registry or a box
    /// catalog refusing it, a name it doesn't know, the network). A failure for the network that
    /// passes (a connection reset, refused or timed out, a gateway's 502, 503 or 504) was tried
    /// again first, up to three more times: the message is the last try's.
    DownloadFailed,
    /// The run is already up.
    AlreadyRunning,
    /// The run is recorded on another place (server or account) than the one asked for.
    RunningElsewhere,
    /// A host port the run publishes is taken.
    PortInUse,
    /// The manifest has no park steps for that mode.
    NotParkable,
    /// Nothing of the run, or not every one of its machines, is left to resume.
    NothingToResume,
    /// The run isn't up.
    NotRunning,
    /// A ready condition didn't hold.
    NotReady,
    /// A required secret or input has no value.
    SecretMissing,
    /// A param is unknown, missing or holds a value it doesn't allow.
    ParamInvalid,
    /// A run name that can't name a run.
    RunName,
    /// The run's Vagrant provider is missing, unknown, not on this host, refused by the run, or
    /// lacks its plugin.
    ProviderRefused,
    /// The elevation Hyper-V's commands need couldn't be had or was lost: the permission prompt
    /// declined or unanswered, the elevated session not started or ended before the operation
    /// (the message says which).
    ElevationFailed,
    /// A machine the run doesn't have.
    MachineUnknown,
    /// The run has several machines the call could mean, and it named none.
    MachineAmbiguous,
    /// A network the run doesn't have.
    NetworkUnknown,
    /// Every slot of the place is held by another run.
    SlotsFull,
    /// A deployment can't be taken over as a run.
    AdoptRefused,
    /// A machine a step goes to over SSH never answered.
    Unreachable,
    /// The run's seal failed: it is up, but not sealed, so not ready.
    NotSealed,
    /// The operation was cut short before it ended (its future dropped: an embedder's cancel, the
    /// command line's Ctrl-C or SIGTERM); the tools it started were asked to stop, and are ended
    /// once they have, or 30 seconds later.
    Cancelled,
    /// The embedder was asked for something that makes no sense (the command line: a run folder
    /// with no manifest, a value missing or malformed), so no operation started.
    Usage,
    /// The machine has no graphical console to reach: its target or provider gives none (the
    /// error says why), it isn't running, or its hypervisor doesn't say where it is.
    NoConsole,
    /// The server the run is on (Proxmox VE, ESXi) refused the run's credentials: its API
    /// answered 401 or 403, or its tool said the sign-in failed (an API token revoked, a password
    /// changed).
    ServerAuthFailed,
    /// A file of the runner's own (a record, a ledger entry) is there but isn't what it wrote:
    /// not JSON, or another version's shape.
    Corrupt,
}

impl ErrorCode {
    /// The code as written in JSON (`port_in_use`).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::ManifestInvalid => "manifest_invalid",
            ErrorCode::ManifestVersion => "manifest_version",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Io => "io",
            ErrorCode::Corrupt => "corrupt",
            ErrorCode::Locked => "locked",
            ErrorCode::ToolMissing => "tool_missing",
            ErrorCode::ToolFailed => "tool_failed",
            ErrorCode::ToolTooOld => "tool_too_old",
            ErrorCode::Timeout => "timeout",
            ErrorCode::DownloadStalled => "download_stalled",
            ErrorCode::DownloadFailed => "download_failed",
            ErrorCode::AlreadyRunning => "already_running",
            ErrorCode::RunningElsewhere => "running_elsewhere",
            ErrorCode::PortInUse => "port_in_use",
            ErrorCode::NotParkable => "not_parkable",
            ErrorCode::NothingToResume => "nothing_to_resume",
            ErrorCode::NotRunning => "not_running",
            ErrorCode::NotReady => "not_ready",
            ErrorCode::SecretMissing => "secret_missing",
            ErrorCode::ParamInvalid => "param_invalid",
            ErrorCode::RunName => "run_name",
            ErrorCode::ProviderRefused => "provider_refused",
            ErrorCode::ElevationFailed => "elevation_failed",
            ErrorCode::MachineUnknown => "machine_unknown",
            ErrorCode::MachineAmbiguous => "machine_ambiguous",
            ErrorCode::NetworkUnknown => "network_unknown",
            ErrorCode::SlotsFull => "slots_full",
            ErrorCode::AdoptRefused => "adopt_refused",
            ErrorCode::Unreachable => "unreachable",
            ErrorCode::NotSealed => "not_sealed",
            ErrorCode::NoConsole => "no_console",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Usage => "usage",
            ErrorCode::ServerAuthFailed => "server_auth_failed",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
