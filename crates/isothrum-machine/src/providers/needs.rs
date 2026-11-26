//! What Vagrant needs on this machine to drive a provider, beyond the hypervisor itself: Vagrant,
//! the provider's plugin, and for VMware's desktop hypervisors HashiCorp's Vagrant VMware Utility
//! (a system service of its own). [`needs`] is the one list readiness ([`super::detect`]) and a
//! setup's steps both read.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::host::{Host, HostOs};

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
    /// HashiCorp's Vagrant VMware Utility, installed and its service running: before the VMware
    /// plugin, which breaks Vagrant for every provider without it
    /// ([`super::Unavailable::VmwarePluginWithoutUtility`]).
    VmwareUtility,
    /// The provider's Vagrant plugin ([`super::ProviderStatus::plugin`]), or a newer one.
    Plugin,
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
    if utility.is_some_and(|u| u != UtilityState::Running) {
        needs.push(Need::VmwareUtility);
    }
    if !plugin_installed {
        needs.push(Need::Plugin);
    }
    needs
}

/// How long the VMware Utility's service gets to accept a connection.
const UTILITY_WAIT: Duration = Duration::from_secs(2);

/// Where the Vagrant VMware Utility stands on `host`: running when its service accepts a
/// connection ([`crate::Devices::vmware_utility_service`], what the plugin talks to, whatever
/// installed it), else installed or not by its marker ([`crate::Devices::vmware_utility`]).
///
/// On Linux it is missing, too, while no `netstat` is on `PATH` ([`crate::Devices::netstat`]):
/// vagrant-vmware-desktop reads the routing table with it before it makes any private network,
/// and stops with "The external program used to read TCP/IP routing tables ... could not be
/// found" without one (Debian 13 and recent Ubuntu and Fedora ship no `net-tools`). The utility's
/// install brings it ([`crate::install::linux::scripts::vmware_utility_script`]), so a setup that
/// installs the utility again is what gives VMware what it needs here.
pub async fn vmware_utility(host: &Host) -> UtilityState {
    if host.os == HostOs::Linux && !host.devices.netstat.iter().any(|p| p.is_file()) {
        return UtilityState::Missing;
    }
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
            [Need::Vagrant, Need::VmwareUtility, Need::Plugin]
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
        given_netstat(&host);
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

    /// A `netstat` where the host looks for one: what vagrant-vmware-desktop needs on Linux.
    fn given_netstat(host: &Host) {
        let netstat = &host.devices.netstat[0];
        std::fs::create_dir_all(netstat.parent().unwrap()).unwrap();
        std::fs::write(netstat, "").unwrap();
    }

    #[tokio::test]
    async fn on_linux_the_utility_is_missing_while_no_netstat_is_there_for_the_plugin() {
        // vagrant-vmware-desktop stopped at its first private network on a Debian 13 without
        // net-tools: "The external program used to read TCP/IP routing tables ... could not be
        // found". Its service running doesn't make VMware ready there.
        let dir = tempfile::tempdir().unwrap();
        let mut host = host(dir.path());
        let marker = &host.devices.vmware_utility;
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(marker, "").unwrap();
        let service = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        host.devices.vmware_utility_service = service.local_addr().unwrap();
        assert_eq!(vmware_utility(&host).await, UtilityState::Missing, "no netstat");
        given_netstat(&host);
        assert_eq!(vmware_utility(&host).await, UtilityState::Running);
        let mut mac = host.clone();
        mac.os = HostOs::Macos;
        std::fs::remove_file(&host.devices.netstat[0]).unwrap();
        assert_eq!(vmware_utility(&mac).await, UtilityState::Running, "Linux's need alone");
    }
}
