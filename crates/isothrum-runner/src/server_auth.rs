//! A server refusing the run's credentials: the API token of a Proxmox VE run revoked, the
//! password of an ESXi host changed. A run on a server (the `proxmox` and `esxi` targets, Vagrant
//! on `vmware_esxi`) whose tool failed in the server's words for a refused sign-in fails with
//! `server_auth_failed` instead of `tool_failed`, so an embedder can say which credentials to
//! update rather than show the tool's output.
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

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, Provider, Target};

use crate::error::Error;

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

/// Whether a run of `manifest` on `provider` is on a server: one whose credentials can be
/// refused.
pub(crate) fn on_server(manifest: &Manifest, provider: Option<Provider>) -> bool {
    matches!(manifest.target, Target::Proxmox | Target::Esxi) || provider == Some(Provider::VmwareEsxi)
}

/// The server a run's `params` name: its Proxmox API's host, or its ESXi host.
pub(crate) fn server_of(params: &BTreeMap<String, String>) -> Option<String> {
    if let Some(endpoint) = params.get("proxmox_endpoint") {
        let rest = endpoint.split_once("://").map_or(endpoint.as_str(), |(_, r)| r);
        let host = rest.split(['/', '?']).next().unwrap_or_default();
        // `[::1]:8006` keeps its brackets' address; `pve:8006` its name.
        let host = match host.strip_prefix('[') {
            Some(v6) => v6.split(']').next().unwrap_or_default(),
            None => host.rsplit_once(':').map_or(host, |(h, _)| h),
        };
        return Some(host.to_owned()).filter(|h| !h.is_empty());
    }
    [isoloom_manifest::contract::ESXI_HOSTNAME, "esxi_hostname"]
        .iter()
        .find_map(|name| params.get(*name))
        .filter(|h| !h.is_empty())
        .cloned()
}

/// `e` as a refused sign-in when it is a step's failure in the server's words for one, on the
/// server `server`; any other error as it is.
pub(crate) fn refusal(e: Error, server: Option<String>) -> Error {
    match e {
        Error::ToolFailed { step, reason } if refused(&reason) => Error::ServerAuthFailed { step, server, reason },
        other => other,
    }
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
    fn the_server_is_named_by_its_api_or_its_host() {
        let params = |k: &str, v: &str| BTreeMap::from([(k.to_owned(), v.to_owned())]);
        assert_eq!(server_of(&params("proxmox_endpoint", "https://pve.lab:8006/")).as_deref(), Some("pve.lab"));
        assert_eq!(server_of(&params("proxmox_endpoint", "https://10.0.0.5")).as_deref(), Some("10.0.0.5"));
        assert_eq!(
            server_of(&params("proxmox_endpoint", "https://[fd00::5]:8006/api2")).as_deref(),
            Some("fd00::5")
        );
        assert_eq!(server_of(&params("ESXI_HOSTNAME", "172.16.1.20")).as_deref(), Some("172.16.1.20"));
        assert_eq!(server_of(&params("esxi_hostname", "esxi.lab")).as_deref(), Some("esxi.lab"));
        assert_eq!(server_of(&params("proxmox_endpoint", "")), None);
        assert_eq!(server_of(&BTreeMap::new()), None);
    }

    #[test]
    fn only_a_steps_failure_in_the_servers_words_is_a_refused_sign_in() {
        let failed = |reason: &str| Error::ToolFailed {
            step: "up".into(),
            reason: reason.into(),
        };
        let e = refusal(failed("received an HTTP 401 response - Reason: authentication failure"), Some("pve.lab".into()));
        assert_eq!(e.code(), crate::ErrorCode::ServerAuthFailed);
        assert_eq!(
            e.to_string(),
            "step `up`: server `pve.lab` refused the run's credentials: received an HTTP 401 response - Reason: authentication failure"
        );
        let other = refusal(failed("exit status 1"), Some("pve.lab".into()));
        assert_eq!(other.code(), crate::ErrorCode::ToolFailed);
        // A box catalog's 403 isn't the server's.
        let download = Error::DownloadFailed {
            step: "boxes-up".into(),
            reason: "The requested URL returned error: 403".into(),
        };
        assert_eq!(refusal(download, None).code(), crate::ErrorCode::DownloadFailed);
        let unnamed = refusal(failed("401 Unauthorized"), None);
        assert!(unnamed.to_string().starts_with("step `up`: the server refused"), "{unnamed}");
    }
}
