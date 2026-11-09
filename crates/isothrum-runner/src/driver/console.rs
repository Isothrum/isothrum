//! What a console into a machine runs once it is in: a login shell on a VM, a shell in a
//! container, and, for a Compose service running inside a VM, the way into its container there;
//! and how such a service is taken out from inside that VM.
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

/// Run inside a VM: Compose service `service`'s container removed, with every container in its
/// network namespace (`network_mode: service:<service>`: part of the same machine), found by its
/// Compose label as [`into_service`] finds it. Nothing to do when it isn't there.
pub(crate) fn remove_service(service: &str) -> String {
    let filter = sh_quote(&format!("label=com.docker.compose.service={service}"));
    format!(
        "set -e; id=$(sudo docker ps -aq --no-trunc --filter {filter} | head -n 1); [ -n \"$id\" ] || exit 0; \
for c in $(sudo docker ps -aq --no-trunc); do \
if [ \"$(sudo docker inspect -f '{{{{.HostConfig.NetworkMode}}}}' \"$c\")\" = \"container:$id\" ]; then sudo docker rm -f \"$c\" >/dev/null; fi; done; \
sudo docker rm -f \"$id\" >/dev/null"
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

    #[test]
    fn a_service_in_a_vm_is_removed_with_what_shares_its_namespace() {
        let script = remove_service("user");
        assert!(script.contains("--filter 'label=com.docker.compose.service=user'"), "{script}");
        assert!(script.contains("= \"container:$id\" ]; then sudo docker rm -f"), "its sidecars: {script}");
        assert!(script.ends_with("sudo docker rm -f \"$id\" >/dev/null"), "{script}");
        assert!(script.contains("[ -n \"$id\" ] || exit 0"), "nothing there: nothing to do");
    }
}
