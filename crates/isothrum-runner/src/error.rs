//! Why an operation failed: one enum for the crate, each variant with a stable [`ErrorCode`].
//! Messages are plain and technical; the embedder maps codes to its own wording.

use std::path::PathBuf;

pub use self::code::ErrorCode;
pub use self::timed_out::TimedOut;

mod code;
mod timed_out;

/// The runner's `Result`: its error is a [`Error`] unless said otherwise.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Why an operation of the runner failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The manifest needs something this runner doesn't do, or the call asks the run for
    /// something its target can't do.
    #[error("{what} isn't supported")]
    Unsupported {
        /// What, in a few words: target `cloud-vm` on this host; step `up`: the action
        /// `ssh.run` on a compose run.
        what: String,
    },
    /// A file or folder couldn't be read or written, or a loopback port bound.
    #[error("{} couldn't be {action}: {source}", path.display())]
    Io {
        /// The file or folder (or the port: `a tcp port on 127.0.0.1`).
        path: PathBuf,
        /// What was done to it (`read`, `written`, `created`, `locked`, `bound`).
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
    /// A tool the run needs is installed, but its probe got no answer from it: a container
    /// engine stopped, or one this account can't reach. Said by its code, so a caller never reads
    /// it out of a [`Error::ToolFailed`]'s wording.
    #[error("step `{step}` failed: `{tool}` doesn't answer: {reason}")]
    ToolUnanswered {
        /// The probing step (`tools`).
        step: String,
        /// The tool, by its manifest name (`docker`).
        tool: String,
        /// What it said, or why its answer is none (secrets masked).
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
    /// A download (what a step downloads in its own phase before it runs, or a step the manifest
    /// marks a download) showed no progress for `after_s` seconds, and was stopped. A download
    /// has no other limit: one still moving runs as long as it moves.
    #[error("step `{step}` showed no progress downloading for {after_s}s and was stopped{}", last_line.as_ref().map_or_else(String::new, |l| format!("; its last line: `{l}`")))]
    DownloadStalled {
        /// The download's step (`pull-up` or `boxes-up`, before the `up` it downloads for; a
        /// manifest's own, `pull-start`).
        step: String,
        /// How long it went without progress, in seconds.
        after_s: u64,
        /// The last line it printed (secrets masked, cut to 300 bytes); none when it printed
        /// nothing.
        last_line: Option<String>,
    },
    /// A download couldn't be made.
    #[error("step `{step}` couldn't download what it needs: {reason}")]
    DownloadFailed {
        /// The download's step (`pull-up`, `boxes-up`, `pull-start`).
        step: String,
        /// The last try's last error lines, or its exit status (secrets masked).
        reason: String,
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
    /// Some of the run's machines are gone (removed outside the runner): a resume brings back
    /// only what was kept, so it can't bring the run back as it was.
    #[error("run `{run}` can't resume as it was: {} gone: start it again", gone(machines))]
    PartlyGone {
        /// The run.
        run: String,
        /// Its machines that are gone.
        machines: Vec<String>,
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
    /// A folder that isn't a project Isothrum runs.
    #[error("{}: not a project to run: {reason}", dir.display())]
    Project {
        /// The folder.
        dir: PathBuf,
        /// Why, with the fix.
        reason: String,
    },
    /// A value or secret that can't be handed to the tools (its name empty or holding `=`, a NUL
    /// in it).
    #[error("input `{name}`: {reason}")]
    Input {
        /// Its name.
        name: String,
        /// Why.
        reason: String,
    },
    /// A machine the run doesn't have (to provision, console into, attach, detach or retire).
    #[error("run `{run}` has no machine `{machine}`")]
    MachineUnknown {
        /// The run.
        run: String,
        /// The machine asked for.
        machine: String,
    },
    /// The run has several machines the call could mean, and it named none.
    #[error("run `{run}` has several {which} ({machines}): name one")]
    MachineAmbiguous {
        /// The run.
        run: String,
        /// Which machines the call could mean (`on-demand machines`).
        which: &'static str,
        /// Their names, comma-separated.
        machines: String,
    },
    /// A network the run doesn't have.
    #[error("run `{run}` has no network `{network}`")]
    NetworkUnknown {
        /// The run.
        run: String,
        /// The network asked for.
        network: String,
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
        /// The provider asked for.
        provider: String,
        /// Why it can't run the run, and what to do.
        reason: String,
    },
    /// A Vagrant run was asked for with no provider.
    #[error("a Vagrant run needs a provider: pass one of this run's ({providers})")]
    NoProvider {
        /// The run's providers, as the manifest names them (`` `virtualbox`, `libvirt` ``).
        providers: String,
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
    #[error(
        "`{value}` can't name a run: use 1 to 63 lowercase letters, digits, `-` or `_`, starting with a letter or digit, not a Windows device name (`con`, `nul`, `com1`…)"
    )]
    RunName {
        /// The name as given.
        value: String,
    },
    /// A machine with no graphical console to reach, and why.
    #[error("machine `{machine}` has no graphical console: {why}")]
    NoConsole {
        /// The machine.
        machine: String,
        /// Why.
        why: String,
    },
    /// The server the run is on refused the run's credentials (a 401 or 403 from its API, its
    /// tool's failed sign-in): an API token revoked, a password changed.
    #[error("step `{step}`: {} refused the run's credentials: {reason}", server.as_ref().map_or_else(|| "the server".to_owned(), |s| format!("server `{s}`")))]
    ServerAuthFailed {
        /// The step (`up`; `status` for a status read).
        step: String,
        /// The server, as the run's params name it (its API's host, the ESXi host), when they do.
        server: Option<String>,
        /// What the server or its tool said (secrets masked).
        reason: String,
    },
    /// A file of the runner's own is there but isn't what it wrote (not JSON, or another
    /// version's shape): told apart from a file that can't be read, which is the OS's to explain.
    #[error("{} holds no {what} this runner reads: {source}", path.display())]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// What it should hold (`record`, `ledger entry`).
        what: &'static str,
        /// Why it doesn't parse.
        #[source]
        source: serde_json::Error,
    },
    /// The home's store couldn't do what was asked.
    #[error("the home's state store couldn't be {action}: {reason}")]
    Store {
        /// What (`opened`, `read`, `written`, `migrated`).
        action: &'static str,
        /// The failure's code (`locked`, `store_corrupt`, `store_version`, `io`).
        code: ErrorCode,
        /// SQLite's word, or the store's.
        reason: String,
    },
    /// The run's state machine refused the move.
    #[error(transparent)]
    IllegalTransition(#[from] crate::state::IllegalTransition),
    /// The run moved between the read and the write that would have moved it.
    #[error("run `{run}` was expected {expected} but is {}", found.map_or("not recorded", crate::state::RunState::as_str))]
    StateConflict {
        /// The run.
        run: String,
        /// Where the move started.
        expected: crate::state::RunState,
        /// Where the run stood instead.
        found: Option<crate::state::RunState>,
    },
    /// A live process holds the lease asked for.
    #[error("`{scope}` is held by process {pid}")]
    LeaseHeld {
        /// The lease.
        scope: String,
        /// Its holder's process id.
        pid: u32,
    },
}

impl Error {
    /// The step the failure happened in, when it names one.
    pub(crate) fn step(&self) -> Option<&str> {
        match self {
            Error::ToolMissing { step, .. }
            | Error::ToolFailed { step, .. }
            | Error::ToolUnanswered { step, .. }
            | Error::Timeout { step, .. }
            | Error::DownloadStalled { step, .. }
            | Error::DownloadFailed { step, .. }
            | Error::ServerAuthFailed { step, .. } => Some(step),
            _ => None,
        }
    }

    /// What ran out of time, for a step that did.
    pub(crate) fn timed_out(&self) -> Option<&TimedOut> {
        match self {
            Error::Timeout { timed_out, .. } => Some(timed_out),
            _ => None,
        }
    }

    /// The failure's stable code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Error::Unsupported { .. } => ErrorCode::Unsupported,
            Error::Project { .. } => ErrorCode::ProjectInvalid,
            Error::Io { .. } => ErrorCode::Io,
            Error::Corrupt { .. } => ErrorCode::Corrupt,
            Error::Locked { .. } => ErrorCode::Locked,
            Error::ToolMissing { .. } => ErrorCode::ToolMissing,
            Error::ToolTooOld { .. } => ErrorCode::ToolTooOld,
            Error::ToolFailed { .. } => ErrorCode::ToolFailed,
            Error::ToolUnanswered { .. } => ErrorCode::ToolUnanswered,
            Error::Timeout { .. } => ErrorCode::Timeout,
            Error::DownloadStalled { .. } => ErrorCode::DownloadStalled,
            Error::DownloadFailed { .. } => ErrorCode::DownloadFailed,
            Error::AlreadyRunning { .. } => ErrorCode::AlreadyRunning,
            Error::RunningElsewhere { .. } => ErrorCode::RunningElsewhere,
            Error::PortInUse { .. } => ErrorCode::PortInUse,
            Error::NotParkable { .. } => ErrorCode::NotParkable,
            Error::NothingToResume { .. } | Error::PartlyGone { .. } => ErrorCode::NothingToResume,
            Error::NotRunning { .. } => ErrorCode::NotRunning,
            Error::NotReady { .. } => ErrorCode::NotReady,
            Error::SecretMissing { .. } => ErrorCode::SecretMissing,
            Error::Input { .. } => ErrorCode::InputInvalid,
            Error::RunName { .. } => ErrorCode::RunName,
            Error::Provider { .. } | Error::NoProvider { .. } => ErrorCode::ProviderRefused,
            Error::Elevation { .. } => ErrorCode::ElevationFailed,
            Error::MachineUnknown { .. } => ErrorCode::MachineUnknown,
            Error::MachineAmbiguous { .. } => ErrorCode::MachineAmbiguous,
            Error::NetworkUnknown { .. } => ErrorCode::NetworkUnknown,
            Error::Adopt { .. } => ErrorCode::AdoptRefused,
            Error::NoConsole { .. } => ErrorCode::NoConsole,
            Error::ServerAuthFailed { .. } => ErrorCode::ServerAuthFailed,
            Error::Store { code, .. } => *code,
            Error::IllegalTransition(_) => ErrorCode::IllegalTransition,
            Error::StateConflict { .. } => ErrorCode::StateConflict,
            Error::LeaseHeld { .. } => ErrorCode::LeaseHeld,
        }
    }

    /// An I/O failure on `path`.
    pub(crate) fn io(path: impl Into<PathBuf>, action: &'static str) -> impl FnOnce(std::io::Error) -> Error {
        let path = path.into();
        move |source| Error::Io { path, action, source }
    }
}

/// Machines `machines` as an error says they are gone: `web is`, `web and db are`, `a, b and c are`.
fn gone(machines: &[String]) -> String {
    let quoted: Vec<String> = machines.iter().map(|m| format!("`{m}`")).collect();
    match quoted.as_slice() {
        [] => "nothing is".to_owned(),
        [one] => format!("machine {one} is"),
        [rest @ .., last] => format!("machines {} and {last} are", rest.join(", ")),
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
    fn a_run_partly_gone_names_its_gone_machines_and_has_nothing_to_resume() {
        let one = Error::PartlyGone {
            run: "r".into(),
            machines: vec!["web".into()],
        };
        assert_eq!(one.code(), ErrorCode::NothingToResume);
        assert_eq!(one.to_string(), "run `r` can't resume as it was: machine `web` is gone: start it again");
        let three = Error::PartlyGone {
            run: "r".into(),
            machines: vec!["a".into(), "b".into(), "c".into()],
        };
        assert!(three.to_string().contains("machines `a`, `b` and `c` are gone"), "{three}");
    }

    #[test]
    fn an_elevation_that_failed_has_its_code_whatever_ended_it() {
        let lost = Error::Elevation {
            reason: "the elevated session ended: the broker stopped".into(),
        };
        assert_eq!((lost.code(), lost.code().as_str()), (ErrorCode::ElevationFailed, "elevation_failed"));
    }
}
