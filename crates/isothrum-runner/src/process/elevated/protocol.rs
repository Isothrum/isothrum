//! The files the runner and its elevated broker talk through, in the session's folder. Both sides
//! take their names from here: the runner's reads and writes, the broker's PowerShell (built by
//! [`super::script`] with the same names) and the tests' fake broker.

use std::fmt::Display;

/// Written by the broker once it takes requests.
pub(super) const READY: &str = "ready";
/// Written by the runner to end the session.
pub(super) const DONE: &str = "done";
/// Written by a broker that stops other than at [`DONE`], holding why.
pub(super) const ENDED: &str = "ended";

/// Request `n`'s program, arguments and folder. `n` is a number on the runner's side and the
/// broker's counter (`$n`) in its PowerShell.
pub(super) fn request(n: impl Display) -> String {
    format!("req-{n}.json")
}

/// Request `n`'s variables.
pub(super) fn variables(n: impl Display) -> String {
    format!("env-{n}.json")
}

/// Request `n`'s output lines.
pub(super) fn output(n: impl Display) -> String {
    format!("out-{n}.log")
}

/// Request `n`'s exit code.
pub(super) fn exit(n: impl Display) -> String {
    format!("exit-{n}")
}

/// Written by the runner to cancel request `n` alone.
pub(super) fn cancel(n: impl Display) -> String {
    format!("cancel-{n}")
}
