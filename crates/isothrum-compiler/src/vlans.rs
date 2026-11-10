//! A switch's VLANs, nested under the LAN named after the switch (Isoloom's `vlans`).

use indexmap::IndexMap;
use isoloom_spec::net::Cidr;

use crate::cables::is_link_network;
use crate::document::{NetworkDoc, ReachDoc, VlanDoc};
use crate::draft::Draft;
use crate::{Error, Lan, Result, Topology};

/// What joins a switch's LAN to the VLAN id in a VLAN's name in the spec.
const VLAN_SEPARATOR: &str = ".vlan";

/// The name a VLAN LAN has in the spec: `<switch>.vlan<id>` (`None` for a plain LAN).
pub(crate) fn vlan_ref(lan: &Lan) -> Option<String> {
    let (switch, id) = vlan_of(lan)?;
    Some(format!("{switch}{VLAN_SEPARATOR}{id}"))
}

/// The switch's LAN and the VLAN id a spec's VLAN name (`sw1.vlan10`) names: [`vlan_ref`]
/// read back. `None` for a name that isn't one.
pub(crate) fn parse_ref(name: &str) -> Option<(&str, u16)> {
    let (lan, id) = name.split_once(VLAN_SEPARATOR)?;
    Some((lan, id.parse().ok()?))
}

/// The switch and VLAN id of a VLAN LAN.
fn vlan_of(lan: &Lan) -> Option<(&str, u16)> {
    Some((lan.switch.as_deref()?, lan.vlan?))
}

/// Refuses a switch named like a plain LAN or like a cable's network: its VLANs nest under a
/// network of the switch's name.
pub(crate) fn check_switch_names(topo: &Topology) -> Result<()> {
    let plain = |name: &String| topo.networks.get(name).is_some_and(|l| vlan_of(l).is_none());
    for switch in topo.networks.values().filter_map(|l| l.switch.as_ref()) {
        if is_link_network(switch) {
            return Err(Error::ReservedName(switch.clone()));
        }
        if plain(switch) {
            return Err(Error::SwitchNameTaken { switch: switch.clone() });
        }
    }
    Ok(())
}

/// The switches' VLANs, nested under each switch's LAN: the LAN's block is the smallest one
/// holding them all, its `internet` the VLANs' when they agree. Machines and `reach` rules name
/// them `<switch>.vlan<id>`.
pub(crate) fn nest(topo: &Topology, draft: &mut Draft) {
    let renamed: IndexMap<&String, String> = topo.networks.iter().filter_map(|(name, lan)| Some((name, vlan_ref(lan)?))).collect();
    if renamed.is_empty() {
        return;
    }
    draft.networks = nested(topo, std::mem::take(&mut draft.networks));
    for parent in draft.networks.values_mut().filter(|n| !n.vlans.is_empty()) {
        settle_parent(parent);
    }
    rename_refs(draft, &renamed);
}

/// The networks with each VLAN moved under its switch's LAN.
fn nested(topo: &Topology, flat: IndexMap<String, NetworkDoc>) -> IndexMap<String, NetworkDoc> {
    let mut networks: IndexMap<String, NetworkDoc> = IndexMap::new();
    for (name, net) in flat {
        let Some((lan, (switch, id))) = topo.networks.get(&name).and_then(|l| Some((l, vlan_of(l)?))) else {
            networks.insert(name, net);
            continue;
        };
        let internet = net.reaches_internet();
        let parent = networks
            .entry(switch.to_owned())
            .or_insert_with(|| NetworkDoc::new(String::new(), internet, None));
        if lan.device.is_some() {
            parent.switch.clone_from(&lan.device);
        }
        parent.vlans.insert(
            id,
            VlanDoc {
                cidr: net.cidr,
                internet: Some(internet),
            },
        );
    }
    networks
}

/// A switch's LAN: the block covering its VLANs, and their `internet` said once when they agree.
fn settle_parent(parent: &mut NetworkDoc) {
    parent.cidr = covering(parent.vlans.values().map(|v| v.cidr.as_str()));
    let all: Vec<bool> = parent.vlans.values().filter_map(|v| v.internet).collect();
    let agreed = match all.split_first() {
        Some((first, rest)) if rest.iter().all(|i| i == first) => Some(*first),
        _ => None,
    };
    // When they disagree, the LAN keeps the default (yes): a VLAN says so only when it differs.
    parent.set_internet(agreed.unwrap_or(true));
    for v in parent.vlans.values_mut().filter(|v| agreed.is_some() || v.internet == Some(true)) {
        v.internet = None;
    }
}

/// Machines' networks and `reach` rules, with each VLAN LAN under its spec name.
fn rename_refs(draft: &mut Draft, renamed: &IndexMap<&String, String>) {
    let rename = |n: &String| renamed.get(n).cloned().unwrap_or_else(|| n.clone());
    for m in draft.machines.values_mut() {
        m.networks = m.networks.iter().map(|(n, o)| (rename(n), *o)).collect();
    }
    for r in &mut draft.reach {
        *r = ReachDoc {
            from: rename(&r.from),
            to: rename(&r.to),
        };
    }
}

/// The smallest IPv4 block holding every one of `cidrs` (at most a /8).
fn covering<'a>(cidrs: impl Iterator<Item = &'a str>) -> String {
    let blocks: Vec<Cidr> = cidrs.filter_map(Cidr::parse).collect();
    let Some(first) = blocks.first() else {
        return String::new();
    };
    let mut len = blocks.iter().map(|b| b.prefix()).min().unwrap_or(24);
    let around = |prefix: u8| Cidr::enclosing(first.address(), prefix);
    while len > 8 && !blocks.iter().all(|b| around(len).contains(*b)) {
        len -= 1;
    }
    around(len).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_covering_block_holds_every_vlan() {
        assert_eq!(covering(["10.10.10.0/24", "10.10.20.0/24"].into_iter()), "10.10.0.0/19");
        assert_eq!(covering(["10.10.10.0/24"].into_iter()), "10.10.10.0/24");
        assert_eq!(covering(["10.10.10.0/24", "11.0.0.0/24"].into_iter()), "10.0.0.0/8", "at most a /8");
        assert_eq!(
            covering(["10.10.10.0/24", "bad", "10.10.11.0/24"].into_iter()),
            "10.10.10.0/23",
            "what isn't a block is left out"
        );
        assert_eq!(covering(std::iter::empty()), "");
    }

    fn vlan(cidr: &str, internet: bool) -> VlanDoc {
        VlanDoc {
            cidr: cidr.into(),
            internet: Some(internet),
        }
    }

    #[test]
    fn vlans_that_agree_say_their_internet_once_on_their_lan() {
        let mut lan = NetworkDoc::new(String::new(), true, None);
        lan.vlans.insert(10, vlan("10.10.10.0/24", false));
        lan.vlans.insert(20, vlan("10.10.20.0/24", false));
        settle_parent(&mut lan);
        assert_eq!((lan.cidr.as_str(), lan.internet), ("10.10.0.0/19", Some(false)));
        assert!(lan.vlans.values().all(|v| v.internet.is_none()));
    }

    #[test]
    fn vlans_that_disagree_keep_the_default_and_the_one_without_says_so() {
        let mut lan = NetworkDoc::new(String::new(), false, None);
        lan.vlans.insert(10, vlan("10.10.10.0/24", true));
        lan.vlans.insert(20, vlan("10.10.20.0/24", false));
        settle_parent(&mut lan);
        assert_eq!(lan.internet, None, "yes, Isoloom's default");
        assert_eq!(lan.vlans.values().map(|v| v.internet).collect::<Vec<_>>(), [None, Some(false)]);
    }

    #[test]
    fn a_vlan_is_named_under_its_switch_and_a_plain_lan_is_not() {
        let lan = |switch: Option<&str>, vlan: Option<u16>| Lan {
            cidr: "10.10.10.0/24".into(),
            internet: true,
            switch: switch.map(str::to_owned),
            vlan,
            device: None,
            tc: None,
        };
        assert_eq!(vlan_ref(&lan(Some("sw1"), Some(10))).as_deref(), Some("sw1.vlan10"));
        assert_eq!(vlan_ref(&lan(Some("sw1"), None)), None);
        assert_eq!(vlan_ref(&lan(None, Some(10))), None);
        assert_eq!(parse_ref("sw1.vlan10"), Some(("sw1", 10)));
        assert_eq!(parse_ref("sw1"), None);
        assert_eq!(parse_ref("sw1.vlanx"), None);
    }
}
