//! What a console into a machine runs once it is in: a login shell on a VM, a shell in a
//! container, and, for a Compose service running inside a VM, the way into its container there.
//! Nothing of a shell is redirected: an interactive shell writes its prompt and what is typed to
//! its error output.

use crate::ssh::sh_quote;

/// A login shell on a VM: bash where it has one.
pub(crate) const LOGIN_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi";

/// A shell in a container: bash where its image has one.
pub(crate) const CONTAINER_SHELL: &str = "if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi";

/// Run inside a VM: a shell in the container of Compose service `service` there, found by its
/// Compose label (whatever project and file started it).
pub(crate) fn into_service(service: &str) -> String {
    let filter = sh_quote(&format!("label=com.docker.compose.service={service}"));
    format!(
        "sudo docker exec -it \"$(sudo docker ps -q --filter {filter} | head -n 1)\" sh -c {}",
        sh_quote(CONTAINER_SHELL)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_in_a_vm_is_entered_by_its_compose_label() {
        assert_eq!(
            into_service("attackbox"),
            "sudo docker exec -it \"$(sudo docker ps -q --filter 'label=com.docker.compose.service=attackbox' | head -n 1)\" sh -c 'if command -v bash >/dev/null 2>&1; then exec bash; else exec sh; fi'"
        );
    }
}
