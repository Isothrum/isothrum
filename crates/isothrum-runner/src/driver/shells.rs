//! The shell scripts a console runs once it is in: a login shell on a VM, a shell in a container.
//! Nothing of a shell is redirected: an interactive shell writes its prompt and what is typed to
//! its error output.

/// A login shell on a VM: bash where it has one.
pub(crate) const LOGIN_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi";

/// A shell in a container: bash where its image has one.
pub(crate) const CONTAINER_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";
