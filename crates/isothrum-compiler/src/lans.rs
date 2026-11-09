//! LANs: each one drawn is a network with its own block, and each machine on it gets its address
//! there, the one it asks for or the next free one.

use indexmap::IndexMap;

use crate::cables::is_link_network;
use crate::document::NetworkDoc;
use crate::draft::Draft;
use crate::{Error, Lan, Result, Topology};

/// The addresses a machine may take on a block of `prefix` bits, lowest first: Isoloom keeps the
/// network address and .1 (the gateway), the two last ones (its controller and router) and the
/// broadcast. A /24 or larger starts at .10, leaving room for the infrastructure people expect
/// low in a LAN.
fn usable(prefix: u8) -> std::ops::RangeInclusive<u8> {
    // A /24 to a /30: 8 to 2 host bits. The block's last address is all of them set.
    let host_bits = 32 - prefix.clamp(24, 30);
    let broadcast = u8::MAX >> (8 - host_bits);
    let first = if host_bits == 8 { 10 } else { 2 };
    first..=broadcast - 3
}

/// The address and prefix length of `a.b.c.d/n`, read leniently: whether the block is one Isoloom
/// takes (its network address, a size it allows) is Isoloom's to say.
pub(crate) fn block(cidr: &str) -> Option<(u32, u8)> {
    let (address, len) = cidr.split_once('/')?;
    let len = len.parse::<u8>().ok().filter(|l| *l <= 32)?;
    Some((u32::from(address.parse::<std::net::Ipv4Addr>().ok()?), len))
}

/// The prefix length of `a.b.c.d/n` ([`block`]).
fn prefix_of(cidr: &str) -> Option<u8> {
    block(cidr).map(|(_, len)| len)
}

/// Each LAN becomes a network, and each machine on it gets its address there.
pub(crate) fn address(topo: &Topology, draft: &mut Draft) -> Result<()> {
    for (name, lan) in &topo.networks {
        address_one(topo, draft, name, lan)?;
    }
    check_known(topo)
}

fn address_one(topo: &Topology, draft: &mut Draft, name: &str, lan: &Lan) -> Result<()> {
    if is_link_network(name) {
        return Err(Error::ReservedLanName(name.into()));
    }
    let prefix = prefix_of(&lan.cidr).ok_or_else(|| Error::BadCidr {
        lan: name.into(),
        cidr: lan.cidr.clone(),
    })?;
    draft
        .networks
        .insert(name.into(), NetworkDoc::new(lan.cidr.clone(), lan.internet, lan.tc.clone()));
    let taken = asked_addresses(topo, draft, name)?;
    let mut free = usable(prefix).filter(|o| !taken.contains_key(o));
    for (node, _) in members(topo, draft, name).into_iter().filter(|(_, octet)| octet.is_none()) {
        let octet = free.next().ok_or_else(|| Error::LanFull {
            lan: name.into(),
            cidr: lan.cidr.clone(),
        })?;
        draft.attach(node, name, octet);
    }
    Ok(())
}

/// The machines on `lan` (automatic routers aside), in the topology's order, each with the
/// octet it asks for there.
fn members<'t>(topo: &'t Topology, draft: &Draft, lan: &str) -> Vec<(&'t str, Option<u8>)> {
    topo.nodes
        .iter()
        .filter(|(node, _)| !draft.is_router(node))
        .filter_map(|(node, n)| n.networks.get(lan).map(|octet| (node.as_str(), *octet)))
        .collect()
}

/// The addresses machines ask for on `lan`, given to them first: octet → machine.
fn asked_addresses(topo: &Topology, draft: &mut Draft, lan: &str) -> Result<IndexMap<u8, String>> {
    let asked: Vec<(&str, u8)> = members(topo, draft, lan).into_iter().filter_map(|(node, octet)| Some((node, octet?))).collect();
    let mut taken: IndexMap<u8, String> = IndexMap::new();
    for (node, octet) in asked {
        if let Some(first) = taken.insert(octet, node.to_owned()) {
            return Err(Error::AddressTaken {
                lan: lan.into(),
                octet,
                first,
                second: node.to_owned(),
            });
        }
        draft.attach(node, lan, octet);
    }
    Ok(taken)
}

/// Refuses a node on a LAN the project doesn't have.
fn check_known(topo: &Topology) -> Result<()> {
    for (node, n) in &topo.nodes {
        if let Some(lan) = n.networks.keys().find(|l| !topo.networks.contains_key(*l)) {
            return Err(Error::UnknownLan {
                node: node.clone(),
                lan: lan.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_offers_what_isoloom_leaves_to_machines() {
        assert_eq!(usable(24), 10..=252);
        assert_eq!(usable(29), 2..=4);
        assert_eq!(usable(16), 10..=252, "larger blocks are read as a /24 of addresses");
        assert_eq!(usable(25), 2..=124);
        assert_eq!(usable(28), 2..=12);
        assert!(usable(30).is_empty(), "a /30 leaves no address to a machine");
        assert_eq!(usable(32), usable(30));
    }

    #[test]
    fn a_cidr_has_four_octets_and_a_prefix() {
        assert_eq!(prefix_of("10.1.2.0/24"), Some(24));
        assert_eq!(prefix_of("10.1.2.0/32"), Some(32));
        for bad in ["10.1.2/24", "office", "10.1.2.0/33", "10.1.2.0", "10.1.2.256/24", "10.1.2.0/x"] {
            assert_eq!(prefix_of(bad), None, "{bad}");
        }
    }
}
