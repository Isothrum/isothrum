//! Where an operation runs: this host's operating system and the run's Vagrant provider. A step,
//! a tool, a param or a slot narrowed to some hosts or providers concerns only those.

use std::collections::BTreeSet;

use isoloom_manifest::{HostOs, Provider};

use crate::encoding::wire_name;

/// Where an operation runs: the host's operating system and the run's Vagrant provider. A step,
/// tool, param or slot narrowed to some hosts or providers concerns only those.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scope {
    /// This host.
    pub(crate) host: HostOs,
    /// The run's provider, when it has one (`${run.provider}`).
    pub(crate) provider: Option<String>,
}

impl Scope {
    /// Whether something for `hosts` and `providers` (empty: every one) concerns this scope.
    pub(crate) fn admits(&self, hosts: &BTreeSet<HostOs>, providers: &BTreeSet<Provider>) -> bool {
        let host = hosts.is_empty() || hosts.contains(&self.host);
        let provider = providers.is_empty() || self.provider.as_deref().is_some_and(|p| providers.iter().any(|q| wire_name(q) == p));
        host && provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_admits_what_names_its_host_and_provider_or_names_none() {
        let esxi = Scope {
            host: HostOs::Macos,
            provider: Some("vmware_esxi".into()),
        };
        let (any_host, any_provider) = (BTreeSet::new(), BTreeSet::new());
        assert!(esxi.admits(&any_host, &any_provider));
        assert!(esxi.admits(&BTreeSet::from([HostOs::Macos]), &BTreeSet::from([Provider::VmwareEsxi])));
        assert!(!esxi.admits(&BTreeSet::from([HostOs::Windows]), &any_provider));
        assert!(!esxi.admits(&any_host, &BTreeSet::from([Provider::Virtualbox])));
        let no_provider = Scope {
            host: HostOs::Linux,
            provider: None,
        };
        assert!(!no_provider.admits(&any_host, &BTreeSet::from([Provider::Virtualbox])));
    }
}
