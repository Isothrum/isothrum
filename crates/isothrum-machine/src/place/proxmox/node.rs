//! A Proxmox VE node's own system, read over SSH ([`Target`]): what its API doesn't say.

use std::time::Duration;

use crate::Context;
use crate::place::ssh::{SshError, Target};

/// How long a read of the node may take.
const READ_LIMIT: Duration = Duration::from_secs(30);

/// Whether the node's `/etc/network/interfaces` sources the SDN configuration
/// (`interfaces.d/*` or `interfaces.d/sdn`): read-only, it changes nothing on the node.
const SDN_SOURCE_CHECK: &str = r"grep -Eq '^[[:space:]]*source[[:space:]]+/etc/network/interfaces\.d/(\*|sdn)([[:space:]]|$)' /etc/network/interfaces";

/// Whether the node loads its SDN configuration: without it, an SDN vnet's bridge is never made
/// and a VM on it fails to start. Proxmox's documented fix is the line
/// `source /etc/network/interfaces.d/*` at the end of `/etc/network/interfaces`, then
/// `ifreload -a`.
///
/// # Errors
///
/// SSH to the node failed (no connection, refused, the host key not confirmed).
pub async fn sdn_sourced(ctx: &Context, node: &Target) -> Result<bool, SshError> {
    sourced(node.exec(ctx, SDN_SOURCE_CHECK, READ_LIMIT).await)
}

/// [`sdn_sourced`] from what the check gave: grep finding no directive fails without a word.
fn sourced(checked: Result<String, SshError>) -> Result<bool, SshError> {
    match checked {
        Ok(_) => Ok(true),
        Err(SshError::Failed(why)) if why.trim() == "exited with code 1" => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether `node` lets its key in: a harmless command run with it.
///
/// # Errors
///
/// Why not: no connection, the key refused, the host key not confirmed.
pub async fn signs_in(ctx: &Context, node: &Target) -> Result<(), SshError> {
    node.exec(ctx, "true", READ_LIMIT).await.map(drop)
}

#[cfg(test)]
mod tests;
