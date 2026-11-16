//! What Vagrant needs on this machine to drive a provider, beyond the hypervisor itself: Vagrant,
//! the provider's plugin, and for VMware's desktop hypervisors HashiCorp's Vagrant VMware Utility
//! (a system service of its own). [`needs`] is the one list readiness ([`super::detect`]) and a
//! setup's steps both read.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::host::Host;

/// What is still missing on this machine for Vagrant to drive a provider, in the order a setup
/// installs it. The one rule both sides read: a provider is ready only when this list is empty
/// (and its hypervisor runs), and a setup offers an install for each entry, so finishing it can't
/// leave the provider short of something no step installs. Exhaustive for that: a setup that
/// matches on it fails to compile until a new one has its step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum Need {
    /// Vagrant itself.
    Vagrant,
    /// The provider's Vagrant plugin ([`super::ProviderStatus::plugin`]), or a newer one.
    Plugin,
    /// HashiCorp's Vagrant VMware Utility, installed and its service running.
    VmwareUtility,
}

/// Where HashiCorp's Vagrant VMware Utility stands on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "ts-rs"), derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum UtilityState {
    /// Not installed.
    Missing,
    /// Installed, but its service doesn't answer.
    Stopped,
    /// Installed, its service answering: what the plugin needs.
    Running,
}

/// What is missing for Vagrant to drive a provider here, from whether Vagrant is installed, its
/// plugin is (true when it needs none), and where the VMware Utility stands (`None` when it needs
/// none). See [`Need`].
#[must_use]
pub fn needs(vagrant_installed: bool, plugin_installed: bool, utility: Option<UtilityState>) -> Vec<Need> {
    let mut needs = Vec::new();
    if !vagrant_installed {
        needs.push(Need::Vagrant);
    }
    if !plugin_installed {
        needs.push(Need::Plugin);
    }
    if utility.is_some_and(|u| u != UtilityState::Running) {
        needs.push(Need::VmwareUtility);
    }
    needs
}

/// How long the VMware Utility's service gets to accept a connection.
const UTILITY_WAIT: Duration = Duration::from_secs(2);

/// Where the Vagrant VMware Utility stands on `host`: running when its service accepts a
/// connection ([`crate::Devices::vmware_utility_service`], what the plugin talks to, whatever
/// installed it), else installed or not by its marker ([`crate::Devices::vmware_utility`]).
pub async fn vmware_utility(host: &Host) -> UtilityState {
    let answers = tokio::time::timeout(UTILITY_WAIT, tokio::net::TcpStream::connect(host.devices.vmware_utility_service)).await;
    if matches!(answers, Ok(Ok(_))) {
        UtilityState::Running
    } else if host.devices.vmware_utility.exists() {
        UtilityState::Stopped
    } else {
        UtilityState::Missing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Linux host whose devices are under `root`.
    fn host(root: &std::path::Path) -> Host {
        crate::testing::linux_host(root)
    }

    #[test]
    fn what_is_needed_is_in_setups_order_and_nothing_when_all_is_there() {
        assert_eq!(
            needs(false, false, Some(UtilityState::Missing)),
            [Need::Vagrant, Need::Plugin, Need::VmwareUtility]
        );
        assert_eq!(needs(true, true, Some(UtilityState::Stopped)), [Need::VmwareUtility]);
        assert_eq!(needs(true, true, Some(UtilityState::Running)), []);
        assert_eq!(needs(true, true, None), []);
        assert_eq!(serde_json::to_value(Need::VmwareUtility).unwrap(), "vmwareUtility");
        assert_eq!(serde_json::to_value(UtilityState::Stopped).unwrap(), "stopped");
    }

    #[tokio::test]
    async fn the_utility_runs_when_its_service_answers_else_its_marker_says_whether_it_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let mut host = host(dir.path());
        assert_eq!(vmware_utility(&host).await, UtilityState::Missing);
        let marker = &host.devices.vmware_utility;
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(marker, "").unwrap();
        assert_eq!(vmware_utility(&host).await, UtilityState::Stopped);
        let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        host.devices.vmware_utility_service = service.local_addr().unwrap();
        assert_eq!(vmware_utility(&host).await, UtilityState::Running);
        std::fs::remove_file(&host.devices.vmware_utility).unwrap();
        assert_eq!(vmware_utility(&host).await, UtilityState::Running, "installed otherwise: it answers");
    }
}
