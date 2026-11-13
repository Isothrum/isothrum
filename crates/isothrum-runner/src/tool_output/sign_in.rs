//! What a server's tools say when it refuses the run's sign-in, read: Terraform's Proxmox and
//! ESXi providers, curl against Proxmox's API, ovftool, the ESXi API and vagrant-vmware-esxi
//! (`server_auth` turns such a step's failure into `server_auth_failed`). Its fixtures are in
//! `fixtures/sign_in/`.
//!
//! What counts as a refused sign-in, in each tool's words:
//!
//! - an HTTP 401 or 403 from the server's API: Terraform's Proxmox provider (`received an HTTP
//!   401 response - Reason: authentication failure`), `curl` (`The requested URL returned error:
//!   401`);
//! - ovftool's and the ESXi API's `incorrect user name or password`;
//! - vagrant-vmware-esxi's SSH to the host (`ESXi host access : Verify …`, `Net::SSH`'s
//!   `Authentication failed for user root@…`), and Go's SSH under Terraform's ESXi provider
//!   (`ssh: unable to authenticate`).

/// What a tool says, lowercased, when the server refused its sign-in.
const REFUSED: [&str; 17] = [
    "http 401",
    "http 403",
    "http/1.1 401",
    "http/1.1 403",
    "http/2 401",
    "http/2 403",
    "returned error: 401",
    "returned error: 403",
    "status code 401",
    "status code 403",
    "401 unauthorized",
    "403 forbidden",
    "incorrect user name or password",
    "incorrect username or password",
    "authentication failed for user",
    "esxi host access : ",
    "ssh: unable to authenticate",
];

/// Whether `text`, what a tool printed, says the server refused its sign-in.
pub(crate) fn refused(text: &str) -> bool {
    let text = text.to_lowercase();
    REFUSED.iter().any(|said| text.contains(said))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_servers_refused_sign_in_is_known_by_its_tools_words() {
        for said in [
            // Terraform's Proxmox provider, a revoked token, then a changed password.
            "Error: error retrieving VM: received an HTTP 401 response - Reason: authentication failure",
            "failed to authenticate: received an HTTP 401 response - Reason: authentication failure",
            "Error: received an HTTP 403 response - Reason: Permission check failed (/vms/104, VM.Audit)",
            // curl against Proxmox's API.
            "curl: (22) The requested URL returned error: 401",
            // ovftool, and the ESXi API.
            "Error: Incorrect user name or password",
            "ServerFaultCode: Cannot complete login due to an incorrect user name or password.",
            // vagrant-vmware-esxi.
            "ESXi host access : Verify password in Vagrantfile is correct.\nUnable to connect to ESXi host! Error: Authentication failed for user root@172.16.1.20",
            // Terraform's ESXi provider.
            "Unable to connect to ESXi host: ssh: handshake failed: ssh: unable to authenticate, attempted methods [none password]",
        ] {
            assert!(refused(said), "{said}");
        }
        for said in [
            "Error: received an HTTP 500 response - Reason: Configuration file 'nodes/pve/qemu-server/104.conf' does not exist",
            "su: Authentication failure",
            "Permission denied (publickey).",
            "dial tcp 10.0.0.5:8006: connect: connection refused",
            "listening on port 4010",
        ] {
            assert!(!refused(said), "{said}");
        }
    }

    #[test]
    fn fixtures_read_as_each_tool_prints_them() {
        assert!(refused(include_str!("fixtures/sign_in/terraform-bpg-proxmox-0.66-token-revoked.txt")));
        assert!(refused(include_str!("fixtures/sign_in/vagrant-vmware-esxi-2.5.5-password-changed.txt")));
        assert!(!refused(include_str!("fixtures/sign_in/terraform-bpg-proxmox-0.66-vm-missing.txt")));
        assert!(!refused(""));
    }
}
