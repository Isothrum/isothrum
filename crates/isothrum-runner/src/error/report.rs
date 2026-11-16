//! A failure as an embedder reads it, the same whichever way it reached it: from the runner's
//! API ([`Error::report`]), from an operation's `error` event, or from the service's refusal.

use serde::{Deserialize, Serialize};

use super::{Error, ErrorCode, ErrorDetail};

/// A failure, as one value: its stable code, its message, the step it happened in, its typed
/// fields, and whether the run was touched. An embedder branches on [`ErrorReport::code`] and
/// reads [`ErrorReport::detail`]'s fields; it never needs the runner's [`Error`] variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[non_exhaustive]
pub struct ErrorReport {
    /// The stable code.
    pub code: ErrorCode,
    /// The message: plain and technical, secrets masked.
    pub message: String,
    /// The step that failed, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    /// The failure's typed fields, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<ErrorDetail>,
    /// Whether the run may have changed: false only when the failure is a refusal made before
    /// anything acted on the run (another operation holds it, it is already up or isn't up, a
    /// value it needs is missing or unusable, a tool it needs is missing), so whatever the caller
    /// prepared for the operation can be put back. Read as true when absent.
    #[serde(default = "touched")]
    pub changed: bool,
}

/// What an absent `changed` reads as: the run may have changed.
fn touched() -> bool {
    true
}

impl ErrorReport {
    /// A failure of `code` saying `message`, with no step and no fields, that may have changed
    /// the run.
    #[must_use]
    pub fn new(code: ErrorCode, message: impl Into<String>) -> ErrorReport {
        ErrorReport {
            code,
            message: message.into(),
            step: None,
            detail: None,
            changed: true,
        }
    }

    /// The same, failed in step `step`.
    #[must_use]
    pub fn with_step(self, step: impl Into<String>) -> ErrorReport {
        ErrorReport {
            step: Some(step.into()),
            ..self
        }
    }

    /// The same, with the typed fields `detail`.
    #[must_use]
    pub fn with_detail(self, detail: ErrorDetail) -> ErrorReport {
        ErrorReport { detail: Some(detail), ..self }
    }

    /// The same, refused before anything acted on the run.
    #[must_use]
    pub fn unchanged(self) -> ErrorReport {
        ErrorReport { changed: false, ..self }
    }

    /// The same, its message and the text its fields carry passed through `mask`.
    #[must_use]
    pub(crate) fn masked(self, mask: impl Fn(&str) -> String) -> ErrorReport {
        ErrorReport {
            message: mask(&self.message),
            detail: self.detail.map(|d| d.masked(&mask)),
            ..self
        }
    }
}

impl std::fmt::Display for ErrorReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error {
    /// The failure as an embedder reads it: [`Error::code`], its message, its step,
    /// [`Error::detail`], and whether the run may have changed.
    #[must_use]
    pub fn report(&self) -> ErrorReport {
        ErrorReport {
            code: self.code(),
            message: self.to_string(),
            step: self.step().map(str::to_owned),
            detail: self.detail(),
            changed: self.changed(),
        }
    }

    /// Whether the run may have changed by this failure: false for the refusals the runner makes
    /// before it acts on a run (its lock held, an `up` of a ready run, a run recorded elsewhere, a
    /// resume of nothing, a change of a run that isn't up, a value missing or unusable, a tool
    /// missing or refusing this account, a cloud refusing the sign-in: a provider's sign-in is
    /// made when Terraform configures it, before it plans or applies anything; an operation the
    /// run's format doesn't do).
    pub(crate) fn changed(&self) -> bool {
        !matches!(
            self,
            Error::Locked { .. }
                | Error::AlreadyRunning { .. }
                | Error::RunningElsewhere { .. }
                | Error::NothingToResume { .. }
                | Error::NotRunning { .. }
                | Error::SecretMissing { .. }
                | Error::Input { .. }
                | Error::ToolMissing { .. }
                | Error::Declined { .. }
                | Error::ToolDenied { .. }
                | Error::CredentialsExpired { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_before_acting_reports_the_run_unchanged_and_a_failed_step_changed() {
        let refused = Error::Locked { run: "r".into() }.report();
        assert_eq!((refused.code, refused.changed), (ErrorCode::Locked, false));
        let failed = Error::ToolFailed {
            step: "up".into(),
            reason: "boom".into(),
        }
        .report();
        assert_eq!(
            (failed.code, failed.step.as_deref(), failed.changed, failed.message.as_str()),
            (ErrorCode::ToolFailed, Some("up"), true, "step `up` failed: boom")
        );
        // A run partly gone was recorded failed: it changed.
        let gone = Error::PartlyGone {
            run: "r".into(),
            machines: vec!["web".into()],
        };
        assert!(gone.report().changed);
    }

    #[test]
    fn a_report_without_changed_reads_as_changed() {
        let read: ErrorReport = serde_json::from_value(serde_json::json!({"code": "io", "message": "m"})).expect("a report");
        assert_eq!(read, ErrorReport::new(ErrorCode::Io, "m"));
        assert!(!read.unchanged().changed);
    }
}
