//! A server refusing the run's credentials: the API token of a Proxmox VE run revoked, the
//! password of an ESXi host changed. A run on a server (the `proxmox` and `esxi` targets, Vagrant
//! on `vmware_esxi`) whose tool failed in the server's words for a refused sign-in fails with
//! `server_auth_failed` instead of `tool_failed`, so an embedder can say which credentials to
//! update rather than show the tool's output. Which words are a refused sign-in,
//! `tool_output::sign_in` reads.

use std::collections::BTreeMap;

use isoloom_manifest::{Manifest, Provider, Target};

use crate::error::Error;
use crate::tool_output::sign_in::refused;

/// Whether a run of `manifest` on `provider` is on a server: one whose credentials can be
/// refused.
pub(crate) fn on_server(manifest: &Manifest, provider: Option<Provider>) -> bool {
    matches!(manifest.target, Target::Proxmox | Target::Esxi) || provider == Some(Provider::VmwareEsxi)
}

/// The server a run's `params` name: its Proxmox API's host, or its ESXi host.
pub(crate) fn server_of(params: &BTreeMap<String, String>) -> Option<String> {
    if let Some(endpoint) = params.get("proxmox_endpoint") {
        let rest = endpoint.split_once("://").map_or(endpoint.as_str(), |(_, r)| r);
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        // A `user:password@` in the address is never part of the name.
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
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
        assert_eq!(
            server_of(&params("proxmox_endpoint", "https://root%40pam:s3cret@pve.lab:8006/")).as_deref(),
            Some("pve.lab"),
            "an address's credentials are never the name"
        );
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
