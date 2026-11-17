//! Why a tool asked a question gave no usable answer: it isn't installed, it gave no answer in
//! time, it couldn't be started, it answered and refused (and how, read from its words), or it
//! answered something that can't be read. Each has a code of the runner's ([`Failure::code`]), so
//! a refused sign-in is [`ErrorCode::CredentialsExpired`] here as it is on a run's step.

use std::time::Duration;

use isothrum_runner::{Captured, ErrorCode, ProcessError, ProcessSpec};

use crate::context::Context;
use crate::exec::{self, Unanswered};

/// How a tool that answered refused, read from what it said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Refusal {
    /// The sign-in its credentials come from ran out or is missing, or the credentials were
    /// rejected: signing in again is the way out.
    CredentialsExpired,
    /// Signed in, but not allowed what was asked: a permission, or an account it can't see.
    Denied,
    /// What was asked about doesn't exist.
    NotFound,
    /// A service the request needs isn't turned on for the account.
    NotEnabled,
    /// A quota of the account is used up.
    QuotaExceeded,
    /// Another refusal: what the tool said is all there is.
    Other,
}

/// Why a tool gave no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Failure {
    /// It isn't installed, or isn't on `PATH`.
    Missing {
        /// The program.
        program: String,
    },
    /// It gave no answer within `limit` (it was ended).
    TimedOut {
        /// The program.
        program: String,
        /// How long it was given.
        limit: Duration,
    },
    /// It couldn't be started for another reason: the OS's.
    NotStarted {
        /// The program.
        program: String,
        /// Why.
        reason: String,
    },
    /// It answered, and refused.
    Refused {
        /// The program.
        program: String,
        /// How, read from what it said.
        refusal: Refusal,
        /// Its reason: its last line on standard error (else on standard output), else how it
        /// ended.
        said: String,
    },
    /// It answered something that can't be read as the answer asked for.
    Unreadable {
        /// The program.
        program: String,
        /// What was wrong with it.
        why: String,
    },
}

impl Failure {
    /// The runner's code for it: a refused sign-in is [`ErrorCode::CredentialsExpired`], a
    /// refused permission [`ErrorCode::ToolDenied`], no answer in time [`ErrorCode::Timeout`].
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        match self {
            Failure::Missing { .. } => ErrorCode::ToolMissing,
            Failure::TimedOut { .. } => ErrorCode::Timeout,
            Failure::NotStarted { .. } | Failure::Unreadable { .. } => ErrorCode::ToolFailed,
            Failure::Refused { refusal, .. } => match refusal {
                Refusal::CredentialsExpired => ErrorCode::CredentialsExpired,
                Refusal::Denied => ErrorCode::ToolDenied,
                Refusal::NotFound | Refusal::NotEnabled | Refusal::QuotaExceeded | Refusal::Other => ErrorCode::ToolFailed,
            },
        }
    }

    /// How the tool refused, when it answered and refused.
    #[must_use]
    pub fn refusal(&self) -> Option<Refusal> {
        match self {
            Failure::Refused { refusal, .. } => Some(*refusal),
            _ => None,
        }
    }

    /// The program that gave no usable answer.
    #[must_use]
    pub fn program(&self) -> &str {
        match self {
            Failure::Missing { program }
            | Failure::TimedOut { program, .. }
            | Failure::NotStarted { program, .. }
            | Failure::Refused { program, .. }
            | Failure::Unreadable { program, .. } => program,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Missing { program } => write!(f, "`{program}` isn't installed or isn't on PATH"),
            Failure::TimedOut { program, limit } => write!(f, "`{program}` gave no answer within {} s", limit.as_secs()),
            Failure::NotStarted { program, reason } => write!(f, "`{program}` couldn't be started: {reason}"),
            Failure::Refused { said, .. } => f.write_str(said),
            Failure::Unreadable { program, why } => write!(f, "`{program}`'s answer couldn't be read: {why}"),
        }
    }
}

impl std::error::Error for Failure {}

/// Runs `spec` for at most `limit`: its standard output when it exits 0, else why not, a refusal
/// read from all it printed by `read`.
///
/// # Errors
///
/// [`Failure`]: it isn't installed, ran out of time, couldn't be started, or refused.
pub(crate) async fn answer(cx: &Context, limit: Duration, spec: &ProcessSpec, read: fn(&str) -> Refusal) -> Result<String, Failure> {
    let program = spec.program.clone();
    let captured = cx.probe_within(limit, spec).await.map_err(|unanswered| match unanswered {
        Unanswered::NotStarted(ProcessError::Missing { .. }) => Failure::Missing { program: program.clone() },
        Unanswered::NotStarted(other) => Failure::NotStarted {
            program: program.clone(),
            reason: other.to_string(),
        },
        Unanswered::TimedOut(given) => Failure::TimedOut {
            program: program.clone(),
            limit: given,
        },
    })?;
    if captured.success() {
        return Ok(captured.stdout.join("\n"));
    }
    Err(Failure::Refused {
        refusal: read(&exec::all_lines(&captured)),
        said: reason(&captured),
        program,
    })
}

/// A failed tool's reason: its last non-blank line on standard error, else on standard output,
/// else how it ended (where tools put the reason last).
fn reason(captured: &Captured) -> String {
    let last = |lines: &[String]| lines.iter().rev().map(|l| l.trim()).find(|l| !l.is_empty()).map(str::to_owned);
    last(&captured.stderr_tail)
        .or_else(|| last(&captured.stdout))
        .unwrap_or_else(|| captured.exit.to_string())
}

/// `program args` within the context's probe time: its standard output when it exits 0, else why
/// not (any refusal [`Refusal::Other`]).
///
/// # Errors
///
/// [`Failure`]: it isn't installed, ran out of time, couldn't be started, or failed.
pub async fn ask(cx: &Context, (program, args): (&str, &[&str])) -> Result<String, Failure> {
    answer(cx, cx.probe_timeout, &ProcessSpec::new(program, args.iter().copied()), |_| Refusal::Other).await
}

#[cfg(test)]
mod tests;
