//! Routing: an automatic router lets every pair of its networks reach each other, both ways; a
//! Cisco router on a LAN is that LAN's gateway.

use isoloom_spec::appliance::is_switch;

use crate::document::ReachDoc;
use crate::draft::Draft;
use crate::vlans::vlan_ref;
use crate::{Kind, Topology};

/// The automatic routers' `reach` rules, each pair once.
pub(crate) fn add_reach_rules(draft: &mut Draft) {
    for nets in draft.routers.values() {
        for from in nets {
            for to in nets {
                let known = draft.reach.iter().any(|r| &r.from == from && &r.to == to);
                if from != to && !known {
                    draft.reach.push(ReachDoc {
                        from: from.clone(),
                        to: to.clone(),
                    });
                }
            }
        }
    }
}

/// A Cisco router on a LAN is that LAN's gateway (at its gateway address): the machines on it
/// route through IOS, as behind a real router. Not on a VLAN: Isoloom's VLANs have no gateway,
/// so there the router stays a member, at the address it already has.
pub(crate) fn cisco_gateways(topo: &Topology, draft: &mut Draft) {
    let plain_lan = |n: &str| topo.networks.get(n).is_some_and(|l| vlan_ref(l).is_none());
    for (lan, net) in draft.networks.iter_mut().filter(|(n, _)| plain_lan(n)) {
        let router = topo
            .nodes
            .iter()
            .find(|(name, n)| is_cisco_router(&n.kind) && draft.machines.get(*name).is_some_and(|m| m.networks.contains_key(lan)));
        if let Some((name, _)) = router
            && let Some(c) = isoloom_spec::net::Cidr::parse(&net.cidr)
            && let Some(m) = draft.machines.get_mut(name)
        {
            net.gateway = Some(name.clone());
            m.networks.insert(lan.clone(), c.gateway_octet());
        }
    }
}

/// A Cisco router appliance: one Isoloom doesn't call a switch ([`is_switch`]).
fn is_cisco_router(kind: &Kind) -> bool {
    matches!(kind, Kind::Container { appliance: Some(a), .. } if !is_switch(*a))
}

#[cfg(test)]
mod tests {
    use isoloom_spec::Appliance;

    use super::*;

    fn container(appliance: Option<Appliance>) -> Kind {
        Kind::Container {
            image: String::new(),
            idle: false,
            appliance,
            config: None,
        }
    }

    #[test]
    fn a_cisco_router_routes_and_a_cisco_switch_or_a_plain_container_does_not() {
        for router in [
            Appliance::CiscoIol,
            Appliance::CiscoVios,
            Appliance::CiscoCsr1000v,
            Appliance::CiscoC8000v,
            Appliance::CiscoDynamips,
        ] {
            assert!(is_cisco_router(&container(Some(router))), "{router:?}");
        }
        assert!(!is_cisco_router(&container(Some(Appliance::CiscoIolL2))));
        assert!(!is_cisco_router(&container(Some(Appliance::CiscoViosL2))));
        assert!(!is_cisco_router(&container(None)));
        assert!(!is_cisco_router(&Kind::Router {}));
    }

    #[test]
    fn each_pair_of_a_routers_networks_reaches_each_other_once() {
        let mut draft = Draft::default();
        draft.routers.insert("r1".into(), vec!["a".into(), "b".into()]);
        draft.routers.insert("r2".into(), vec!["b".into(), "a".into(), "c".into()]);
        add_reach_rules(&mut draft);
        let pairs: Vec<(&str, &str)> = draft.reach.iter().map(|r| (r.from.as_str(), r.to.as_str())).collect();
        assert_eq!(pairs, [("a", "b"), ("b", "a"), ("b", "c"), ("a", "c"), ("c", "b"), ("c", "a")]);
    }
}
