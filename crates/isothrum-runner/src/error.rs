//! Why an operation failed: one enum for the crate, each variant with a stable [`ErrorCode`].
//! Messages are plain and technical; the embedder maps codes to its own wording.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub use self::timed_out::TimedOut;

mod timed_out;

/// The stable code of a failure, the same in the Rust API and the event stream (`error.code`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// The manifest isn't valid JSON or breaks the contract.
    ManifestInvalid,
    /// The manifest is another version of the contract.
    ManifestVersion,
    /// The manifest needs something this runner doesn't do yet (a driver, a delivery, a condition).
    Unsupported,
    /// A file or folder couldn't be read or written.
    Io,
    /// Another process runs an operation on this run.
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
    /// The run is already up.
    AlreadyRunning,
    /// The run is recorded on another place (server or account) than the one asked for.
    RunningElsewhere,
    /// A host port the run publishes is taken.
    PortInUse,
    /// The manifest has no park steps for that mode.
    NotParkable,
    /// Nothing of the run is left to resume.
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
    /// The elevation Hyper-V's commands need couldn't be had (the permission prompt declined).
    ElevationDeclined,
    /// A machine the run doesn't have.
    MachineUnknown,
    /// Every slot of the place is held by another run.
    SlotsFull,
    /// A deployment can't be taken over as a run.
    AdoptRefused,
    /// A machine a step goes to over SSH never answered.
    Unreachable,
    /// The run's seal failed: it is up, but not sealed, so not ready.
    NotSealed,
    /// The operation was cut short before it ended (its future dropped: an embedder's cancel, the
    /// command line's Ctrl-C or SIGTERM); the tools it started were stopped with it.
    Cancelled,
    /// The embedder was asked for something that makes no sense (the command line: a run folder
    /// with no manifest, a value missing or malformed), so no operation started.
    Usage,
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
            ErrorCode::Locked => "locked",
            ErrorCode::ToolMissing => "tool_missing",
            ErrorCode::ToolFailed => "tool_failed",
            ErrorCode::ToolTooOld => "tool_too_old",
            ErrorCode::Timeout => "timeout",
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
            ErrorCode::ElevationDeclined => "elevation_declined",
            ErrorCode::MachineUnknown => "machine_unknown",
            ErrorCode::SlotsFull => "slots_full",
            ErrorCode::AdoptRefused => "adopt_refused",
            ErrorCode::Unreachable => "unreachable",
            ErrorCode::NotSealed => "not_sealed",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Usage => "usage",
        }
    }
}

/// The runner's `Result`: its error is a [`Error`] unless said otherwise.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why an operation of the runner failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The manifest was refused (its own error names the field and the fix).
    #[error("{}: {source}", path.display())]
    Manifest {
        /// The manifest's file.
        path: PathBuf,
        /// Why, with Isoloom's stable code (`source.code()`).
        source: isoloom_manifest::Error,
    },
    /// The manifest needs something this runner doesn't do yet.
    #[error("{what} isn't supported by this runner yet")]
    Unsupported {
        /// What, as the manifest names it (`driver vagrant`, `delivery tf_var`).
        what: String,
    },
    /// A file or folder couldn't be read or written.
    #[error("{} couldn't be {action}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What was done to it (`read`, `written`, `created`, `locked`).
        action: &'static str,
        /// The OS's reason.
        source: std::io::Error,
    },
    /// Another process holds the run's lock.
    #[error("run `{run}` is busy: another process runs an operation on it")]
    Locked {
        /// The run.
        run: String,
    },
    /// A step's tool isn't on PATH.
    #[error("step `{step}`: `{tool}` isn't installed or isn't on PATH")]
    ToolMissing {
        /// The step.
        step: String,
        /// The tool.
        tool: String,
    },
    /// A tool answers at a version older than the manifest's `min_version` for it.
    #[error("`{tool}` is version {found}; this run needs {required} or later")]
    ToolTooOld {
        /// The tool.
        tool: String,
        /// The version it answered with.
        found: String,
        /// The oldest version the run takes.
        required: String,
    },
    /// A step's tool ran and failed.
    #[error("step `{step}` failed: {reason}")]
    ToolFailed {
        /// The step.
        step: String,
        /// Its last error lines, or its exit status (secrets masked).
        reason: String,
    },
    /// A step ran out of time: it showed no progress for its limit, or it ran to the ceiling.
    #[error("step `{step}` {timed_out}")]
    Timeout {
        /// The step.
        step: String,
        /// Which limit it ran into.
        timed_out: TimedOut,
    },
    /// The run is already up.
    #[error("run `{run}` is already running")]
    AlreadyRunning {
        /// The run.
        run: String,
    },
    /// The run is recorded somewhere else.
    #[error("run `{run}` is recorded on `{recorded}`, not `{asked}`")]
    RunningElsewhere {
        /// The run.
        run: String,
        /// Where its record says it is.
        recorded: String,
        /// Where this operation was asked to run.
        asked: String,
    },
    /// A host port the run publishes is taken.
    #[error("host port {port}/{protocol} is in use")]
    PortInUse {
        /// The port.
        port: u16,
        /// `tcp` or `udp`.
        protocol: String,
    },
    /// No park steps for that mode.
    #[error("run `{run}` can't be parked ({mode}): its manifest has no steps for it")]
    NotParkable {
        /// The run.
        run: String,
        /// The mode asked for.
        mode: &'static str,
    },
    /// Nothing left to resume.
    #[error("run `{run}` has nothing left to resume: start it again")]
    NothingToResume {
        /// The run.
        run: String,
    },
    /// The run isn't up.
    #[error("run `{run}` isn't running")]
    NotRunning {
        /// The run.
        run: String,
    },
    /// A ready condition didn't hold.
    #[error("not ready: {condition}: {detail}")]
    NotReady {
        /// The condition (`step_ok up`, `tcp 127.0.0.1:8080`).
        condition: String,
        /// Why.
        detail: String,
    },
    /// A required secret or input has no value.
    #[error("secret `{name}` is required and has no value")]
    SecretMissing {
        /// The slot.
        name: String,
    },
    /// A param that can't be used as given.
    #[error("param `{name}`: {reason}")]
    Param {
        /// The param.
        name: String,
        /// Why.
        reason: String,
    },
    /// A placeholder with no value at run time.
    #[error("{at}: `${{{placeholder}}}` has no value in this operation")]
    Unbound {
        /// Where (`lifecycle.up.up`).
        at: String,
        /// The placeholder.
        placeholder: String,
    },
    /// A machine the run doesn't have (to provision).
    #[error("run `{run}` has no machine `{machine}`")]
    MachineUnknown {
        /// The run.
        run: String,
        /// The machine asked for.
        machine: String,
    },
    /// The elevation the run's commands need couldn't be had.
    #[error("elevation: {reason}")]
    Elevation {
        /// Why (the permission prompt declined, the elevated session ended).
        reason: String,
    },
    /// The run's Vagrant provider can't run it.
    #[error("provider `{provider}`: {reason}")]
    Provider {
        /// The provider asked for (empty when none was).
        provider: String,
        /// Why it can't run the run, and what to do.
        reason: String,
    },
    /// Every slot of the run's place is held by another run.
    #[error("no slot is free on `{place}`: {held} runs hold one; stop one first")]
    SlotsFull {
        /// The place (the embedder's label for it, or `this host`).
        place: String,
        /// How many runs hold a slot there.
        held: usize,
    },
    /// A machine a step goes to over SSH never answered within the step's `wait_s`.
    #[error("step `{step}`: {login} didn't answer over SSH within {after_s}s: {reason}")]
    Unreachable {
        /// The step.
        step: String,
        /// The login it tried (`user@host`).
        login: String,
        /// How long it tried, in seconds.
        after_s: u64,
        /// ssh's last word.
        reason: String,
    },
    /// The run's seal failed: it is up, unsealed, and never reported ready.
    #[error("run `{run}` couldn't be sealed, so it isn't ready (stop it to remove it): {reason}")]
    Unsealed {
        /// The run.
        run: String,
        /// Why the seal failed.
        reason: String,
    },
    /// A deployment that can't be taken over as a run.
    #[error("run `{run}` can't take the deployment over: {reason}")]
    Adopt {
        /// The run it would have become.
        run: String,
        /// Why.
        reason: String,
    },
    /// A run name that can't name a run.
    #[error("`{value}` can't name a run: use 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit")]
    RunName {
        /// The name as given.
        value: String,
    },
}

impl Error {
    /// The failure's stable code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Manifest { source, .. } if source.code() == "version" => ErrorCode::ManifestVersion,
            Error::Manifest { .. } => ErrorCode::ManifestInvalid,
            Error::Unsupported { .. } | Error::Unbound { .. } => ErrorCode::Unsupported,
            Error::Io { .. } => ErrorCode::Io,
            Error::Locked { .. } => ErrorCode::Locked,
            Error::ToolMissing { .. } => ErrorCode::ToolMissing,
            Error::ToolTooOld { .. } => ErrorCode::ToolTooOld,
            Error::ToolFailed { .. } => ErrorCode::ToolFailed,
            Error::Timeout { .. } => ErrorCode::Timeout,
            Error::AlreadyRunning { .. } => ErrorCode::AlreadyRunning,
            Error::RunningElsewhere { .. } => ErrorCode::RunningElsewhere,
            Error::PortInUse { .. } => ErrorCode::PortInUse,
            Error::NotParkable { .. } => ErrorCode::NotParkable,
            Error::NothingToResume { .. } => ErrorCode::NothingToResume,
            Error::NotRunning { .. } => ErrorCode::NotRunning,
            Error::NotReady { .. } => ErrorCode::NotReady,
            Error::SecretMissing { .. } => ErrorCode::SecretMissing,
            Error::Param { .. } => ErrorCode::ParamInvalid,
            Error::RunName { .. } => ErrorCode::RunName,
            Error::Provider { .. } => ErrorCode::ProviderRefused,
            Error::Elevation { .. } => ErrorCode::ElevationDeclined,
            Error::MachineUnknown { .. } => ErrorCode::MachineUnknown,
            Error::SlotsFull { .. } => ErrorCode::SlotsFull,
            Error::Adopt { .. } => ErrorCode::AdoptRefused,
            Error::Unreachable { .. } => ErrorCode::Unreachable,
            Error::Unsealed { .. } => ErrorCode::NotSealed,
        }
    }

    /// An I/O failure on `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, action: &'static str) -> impl FnOnce(std::io::Error) -> Error {
        let path = path.into();
        move |source| Error::Io { path, action, source }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_is_written_as_serde_writes_it() {
        crate::testing::assert_named_as_serde(ErrorCode::as_str);
    }

    #[test]
    fn another_manifest_version_has_its_own_code() {
        let err = isoloom_manifest::parse(r#"{"manifest_version": 1}"#).unwrap_err();
        let err = Error::Manifest {
            path: "run.json".into(),
            source: err,
        };
        assert_eq!(err.code(), ErrorCode::ManifestVersion);
        assert!(err.to_string().starts_with("run.json: manifest_version 1"), "{err}");
    }
}
